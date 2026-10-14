import Foundation
import Network
import Security
import CryptoKit

// Data connections for the runner forward channel (docs/design/self-runner-remote.md, P1).
// Each tunnel uses its own LAN TLS or WSS/E2E connection, authenticated with a separate MAC
// context ("selfrunner-tunnel-v1"), so it can never be taken for the control link and the control
// link's wire format stays untouched. The gateway pairs an idle tunnel with a Mac-local TCP
// connection by sending {"t":"open","port":P}; after {"t":"opened","ok":true} both sides copy raw
// bytes. All state lives on main, like RemoteLink.
final class RemoteTunnel {
    // The daemon opens a fresh connection per runner request, so a batch drains the pool fast. Over
    // WSS a tunnel takes 1–2 s to come up (TLS, upgrade, E2E, challenge), hence a deeper pool and up to
    // `parallelDials` handshakes at once while healthy; after a failure dialling is serial again.
    static let idleTarget = 4
    static let totalLimit = 10
    private static let parallelDials = 3
    private static let preAuthLimit = 4096
    private static let dialSpacing = 0.3
    private static let localConnectTimeout = 1.5
    // The gateway drops idle tunnels after 120 s. Recycling a little earlier on our side bounds how long
    // a tunnel whose path died silently (Wi‑Fi -> cellular: the gateway's FIN never arrives) can sit
    // in the pool looking idle while the gateway has none.
    private static let idleRecycle = 110.0

    private enum Phase { case connecting, challenge, welcome, idle, opening, piping }
    private final class Tunnel {
        let id: Int
        let outer: RemoteTransport
        var budget: DispatchWorkItem?
        var remainingRoutes: [String] = []
        var runnerEOF = false
        var macEOF = false
        var local: NWConnection?
        var phase = Phase.connecting
        var buffer = Data()
        init(id: Int, outer: RemoteTransport) { self.id = id; self.outer = outer }
    }

    private let routes = RemoteRoutePolicy()
    private var enabled = false
    private var pairing: RemotePairing?
    private var epoch = 0
    private var nextID = 0
    private var tunnels: [Int: Tunnel] = [:]
    private var lastDial = Date.distantPast
    private var backoff = 0.0
    private var scheduled: DispatchWorkItem?
    var log: (String) -> Void = { _ in }
    /// Ports the gateway may open (runner API, MJPEG/H.264) -> the 127.0.0.1 port actually dialled.
    /// Anything else is refused. Set from RunnerCore's real ports once they are configurable.
    var ports: [UInt16: UInt16] = [8100: 8100, 9100: 9100]

    /// Tunnels waiting for an open (including ones still handshaking) and tunnels carrying a stream.
    var status: [String: Int] {
        let idle = tunnels.values.filter { $0.phase != .opening && $0.phase != .piping }.count
        return ["idle": idle, "active": tunnels.count - idle]
    }

    init() { routes.changed = { [weak self] in self?.pathChanged() } }

    // Idle tunnels were dialled over the old path and may be dead without any notice; replace them.
    private func pathChanged() {
        for tunnel in tunnels.values where tunnel.phase == .idle { close(tunnel, reason: "path_changed") }
        fill()
    }

    func configure(enabled: Bool, pairing: RemotePairing?) {
        routes.reset()
        self.enabled = enabled; self.pairing = pairing
        epoch += 1
        scheduled?.cancel(); scheduled = nil
        for tunnel in tunnels.values { tunnel.budget?.cancel(); tunnel.local?.cancel(); tunnel.outer.cancel() }
        tunnels.removeAll()
        backoff = 0
        fill()
    }

    private func fill() {
        let handshaking = tunnels.values.filter { [.connecting, .challenge, .welcome].contains($0.phase) }.count
        guard enabled, pairing != nil, scheduled == nil, handshaking < (backoff > 0 ? 1 : Self.parallelDials) else { return }
        let waiting = tunnels.values.filter { $0.phase != .opening && $0.phase != .piping }.count
        guard waiting < Self.idleTarget, tunnels.count < Self.totalLimit else { return }
        let earliest = lastDial.addingTimeInterval(max(Self.dialSpacing, backoff))
        let delay = earliest.timeIntervalSinceNow
        if delay > 0 {
            let token = epoch
            let item = DispatchWorkItem { [weak self] in
                guard let self, token == self.epoch else { return }
                self.scheduled = nil; self.fill()
            }
            scheduled = item
            DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: item)
            return
        }
        dial()
    }

    private func dial() {
        guard let pair = pairing else { return }
        dial(routes.routes(pair))
    }
    private func dial(_ candidates: [String]) {
        guard let pair = pairing, let route = candidates.first else { return }
        nextID += 1
        let token = epoch
        let tunnel = Tunnel(id: nextID, outer: RemoteRoutePolicy.make(pair, route: route))
        tunnel.remainingRoutes = Array(candidates.dropFirst())
        tunnels[tunnel.id] = tunnel
        lastDial = Date()
        tunnel.outer.ready = { [weak self, weak tunnel] in
            guard let self, let tunnel, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            tunnel.phase = .challenge; self.receiveFrames(tunnel, token: token)
        }
        tunnel.outer.failed = { [weak self, weak tunnel] _ in
            guard let self, let tunnel, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            self.close(tunnel, reason: "connect")
        }
        let timer = DispatchWorkItem { [weak self, weak tunnel] in
            guard let self, let tunnel, token == self.epoch, self.tunnels[tunnel.id] === tunnel,
                  [.connecting, .challenge, .welcome].contains(tunnel.phase) else { return }
            self.close(tunnel, reason: "handshake_timeout")
        }
        tunnel.budget = timer
        DispatchQueue.main.asyncAfter(deadline: .now() + (route == "lan" ? 2 : 8), execute: timer)
        tunnel.outer.start()
    }

    // Length-prefixed JSON frames until "open"; raw bytes after "opened".
    private func receiveFrames(_ tunnel: Tunnel, token: Int) {
        tunnel.outer.receive { [weak self] data, end, error in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            if let data { tunnel.buffer.append(data) }
            while tunnel.phase != .opening, tunnel.buffer.count >= 4 {
                let length = tunnel.buffer.prefix(4).reduce(0) { ($0 << 8) | Int($1) }
                guard length > 0, length <= Self.preAuthLimit else { self.close(tunnel, reason: "frame"); return }
                guard tunnel.buffer.count >= length + 4 else { break }
                let bytes = tunnel.buffer.subdata(in: 4..<(length + 4))
                tunnel.buffer = Data(tunnel.buffer.dropFirst(length + 4))
                guard let frame = try? JSONSerialization.jsonObject(with: bytes) as? [String: Any],
                      self.handle(frame, tunnel: tunnel, token: token) else { self.close(tunnel, reason: "protocol"); return }
            }
            // The gateway sends nothing between "open" and our "opened"; stop reading so the stream's
            // first bytes are left to the pipe.
            if tunnel.phase == .opening { return }
            if end || error != nil { self.close(tunnel, reason: tunnel.phase == .idle ? "idle_closed" : "handshake"); return }
            self.receiveFrames(tunnel, token: token)
        }
    }

    private func handle(_ frame: [String: Any], tunnel: Tunnel, token: Int) -> Bool {
        let t = frame["t"] as? String
        switch tunnel.phase {
        case .challenge:
            guard t == "challenge", let nonce = frame["nonce"] as? String,
                  let data = Data(base64Encoded: nonce), data.count == 32, let pair = pairing else { return false }
            let message = Data("selfrunner-tunnel-v1".utf8) + data + Data(pair.device_id.utf8)
            let mac = Data(HMAC<SHA256>.authenticationCode(for: message, using: SymmetricKey(data: pair.key)))
            tunnel.phase = .welcome
            sendFrame(["t": "hello", "v": 1, "role": "tunnel", "device_id": pair.device_id,
                       "mac": mac.base64EncodedString()], tunnel: tunnel)
            return true
        case .welcome:
            guard t == "welcome" else { return false }
            tunnel.budget?.cancel(); tunnel.budget = nil
            tunnel.phase = .idle
            DispatchQueue.main.asyncAfter(deadline: .now() + Self.idleRecycle) { [weak self, weak tunnel] in
                guard let self, let tunnel, token == self.epoch, self.tunnels[tunnel.id] === tunnel,
                      tunnel.phase == .idle else { return }
                self.close(tunnel, reason: "idle_recycle")
            }
            backoff = 0
            fill()
            return true
        case .idle:
            guard t == "open", let raw = frame["port"] as? Int, let port = UInt16(exactly: raw) else { return false }
            tunnel.phase = .opening
            fill()
            guard let target = ports[port] else {
                log("tunnel \(tunnel.id): open refused port \(port)")
                sendFrame(["t": "opened", "ok": false], tunnel: tunnel) { [weak self] in self?.close(tunnel, reason: "port_refused") }
                return true
            }
            openLocal(tunnel, port: target, token: token)
            return true
        default:
            return false
        }
    }

    private func openLocal(_ tunnel: Tunnel, port: UInt16, token: Int) {
        let local = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        tunnel.local = local
        local.stateUpdateHandler = { [weak self] state in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            switch state {
            case .ready where tunnel.phase == .opening:
                tunnel.phase = .piping
                self.log("tunnel \(tunnel.id): opened port \(port)")
                self.sendFrame(["t": "opened", "ok": true], tunnel: tunnel) { [weak self] in
                    guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                    let early = tunnel.buffer; tunnel.buffer = Data()
                    if early.isEmpty {
                        self.pumpOuter(tunnel, local: local, token: token)
                    } else {
                        local.send(content: early, completion: .contentProcessed { [weak self] error in
                            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                            if error != nil { self.close(tunnel, reason: "local_send"); return }
                            self.pumpOuter(tunnel, local: local, token: token)
                        })
                    }
                    self.pumpLocal(tunnel, local: local, token: token)
                }
            case .failed, .waiting, .cancelled:
                if tunnel.phase == .opening { self.refuseOpen(tunnel, reason: "local_connect") }
            // While piping, the pumps decide: a state change can arrive before the last received
            // bytes have been forwarded, and closing here would drop them.
            default: break
            }
        }
        local.start(queue: .main)
        // The gateway gives up after its tunnel_open (2 s); answer before that.
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.localConnectTimeout) { [weak self] in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel, tunnel.phase == .opening else { return }
            self.refuseOpen(tunnel, reason: "local_timeout")
        }
    }

    private func refuseOpen(_ tunnel: Tunnel, reason: String) {
        guard tunnel.phase == .opening else { return }
        tunnel.phase = .piping // no second answer
        log("tunnel \(tunnel.id): open failed (\(reason))")
        sendFrame(["t": "opened", "ok": false], tunnel: tunnel) { [weak self] in self?.close(tunnel, reason: reason) }
    }

    // Both pumps wait for send completion before reading again (bounded backpressure).
    private func pumpOuter(_ tunnel: Tunnel, local: NWConnection, token: Int) {
        tunnel.outer.receive { [weak self] data, end, error in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            if error != nil { self.close(tunnel, reason: "pipe_error"); return }
            local.send(content: data, contentContext: end ? .finalMessage : .defaultMessage, isComplete: true,
                       completion: .contentProcessed { [weak self] error in
                guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                if error != nil { self.close(tunnel, reason: "pipe_error"); return }
                if end {
                    tunnel.macEOF = true
                    if tunnel.runnerEOF { self.close(tunnel, reason: "done") }
                } else { self.pumpOuter(tunnel, local: local, token: token) }
            })
        }
    }
    private func pumpLocal(_ tunnel: Tunnel, local: NWConnection, token: Int) {
        local.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, end, error in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            if error != nil { self.close(tunnel, reason: "pipe_error"); return }
            let forwarded: (Error?) -> Void = { [weak self] error in
                guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                if error != nil { self.close(tunnel, reason: "pipe_error"); return }
                if end {
                    if tunnel.outer.route == "lan" { self.close(tunnel, reason: "done"); return }
                    tunnel.outer.finish { [weak self] error in
                        guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                        if error != nil { self.close(tunnel, reason: "pipe_error"); return }
                        tunnel.runnerEOF = true
                        if tunnel.macEOF { self.close(tunnel, reason: "done") }
                    }
                } else { self.pumpLocal(tunnel, local: local, token: token) }
            }
            if let data, !data.isEmpty { tunnel.outer.send(data, completion: forwarded) }
            else { forwarded(nil) }
        }
    }

    private func sendFrame(_ frame: [String: Any], tunnel: Tunnel, then: (() -> Void)? = nil) {
        guard let data = try? JSONSerialization.data(withJSONObject: frame), data.count <= Self.preAuthLimit else {
            close(tunnel, reason: "frame"); return
        }
        let n = UInt32(data.count)
        let header = Data([UInt8((n >> 24) & 255), UInt8((n >> 16) & 255), UInt8((n >> 8) & 255), UInt8(n & 255)])
        let token = epoch
        tunnel.outer.send(header + data) { [weak self] error in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            if error != nil { self.close(tunnel, reason: "send"); return }
            then?()
        }
    }

    private func close(_ tunnel: Tunnel, reason: String) {
        guard tunnels[tunnel.id] === tunnel else { return }
        tunnels[tunnel.id] = nil
        let beforeIdle = [.connecting, .challenge, .welcome].contains(tunnel.phase)
        tunnel.budget?.cancel(); tunnel.budget = nil
        tunnel.local?.cancel(); tunnel.outer.cancel()
        if beforeIdle {
            routes.failure(tunnel.outer.route)
            if !tunnel.remainingRoutes.isEmpty {
                // Preserve §13's minimum dial spacing even when trying the other route.
                let token = epoch
                let candidates = tunnel.remainingRoutes
                let item = DispatchWorkItem { [weak self] in
                    guard let self, token == self.epoch else { return }
                    self.scheduled = nil; self.dial(candidates)
                }
                scheduled = item
                DispatchQueue.main.asyncAfter(deadline: .now() + max(0, lastDial.addingTimeInterval(Self.dialSpacing).timeIntervalSinceNow), execute: item)
                return
            }
            // Authentication or reachability failure: back off. Normal recycling (the gateway's idle
            // timeout, a finished stream) refills at the plain dial spacing.
            backoff = min(30, max(1, backoff * 2))
            log("tunnel \(tunnel.id): \(reason), retry in \(Int(backoff))s")
        } else if !["done", "idle_closed", "idle_recycle"].contains(reason) {
            log("tunnel \(tunnel.id): closed (\(reason))")
        }
        fill()
    }
}
