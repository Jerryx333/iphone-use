import Foundation
import Network
import Security
import CryptoKit

// Data connections for the runner forward channel (docs/design/self-runner-remote.md, P1).
// Each tunnel is its own TLS connection to the gateway port, authenticated with a separate MAC
// context ("selfrunner-tunnel-v1"), so it can never be taken for the control link and the control
// link's wire format stays untouched. The gateway pairs an idle tunnel with a Mac-local TCP
// connection by sending {"t":"open","port":P}; after {"t":"opened","ok":true} both sides copy raw
// bytes. All state lives on main, like RemoteLink.
final class RemoteTunnel {
    static let idleTarget = 2
    static let totalLimit = 6
    private static let preAuthLimit = 4096
    private static let dialSpacing = 0.3
    private static let handshakeTimeout = 5.0
    private static let localConnectTimeout = 1.5

    private enum Phase { case connecting, challenge, welcome, idle, opening, piping }
    private final class Tunnel {
        let id: Int
        let outer: NWConnection
        var local: NWConnection?
        var phase = Phase.connecting
        var buffer = Data()
        init(id: Int, outer: NWConnection) { self.id = id; self.outer = outer }
    }

    private var enabled = false
    private var pairing: RemotePairing?
    private var epoch = 0
    private var nextID = 0
    private var tunnels: [Int: Tunnel] = [:]
    private var dialing = false
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

    init() {}

    func configure(enabled: Bool, pairing: RemotePairing?) {
        self.enabled = enabled; self.pairing = pairing
        epoch += 1
        scheduled?.cancel(); scheduled = nil
        for tunnel in tunnels.values { tunnel.local?.cancel(); tunnel.outer.cancel() }
        tunnels.removeAll()
        dialing = false; backoff = 0
        fill()
    }

    private func fill() {
        guard enabled, pairing != nil, !dialing, scheduled == nil else { return }
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
        nextID += 1
        let token = epoch
        let tunnel = Tunnel(id: nextID, outer: NWConnection(host: NWEndpoint.Host(pair.host),
            port: NWEndpoint.Port(rawValue: pair.port)!, using: Self.parameters(pin: pair.pin)))
        tunnels[tunnel.id] = tunnel
        dialing = true; lastDial = Date()
        tunnel.outer.stateUpdateHandler = { [weak self] state in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            switch state {
            case .ready: tunnel.phase = .challenge; self.receiveFrames(tunnel, token: token)
            case .failed, .cancelled, .waiting: self.close(tunnel, reason: "connect")
            default: break
            }
        }
        tunnel.outer.start(queue: .main)
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.handshakeTimeout) { [weak self] in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel,
                  [.connecting, .challenge, .welcome].contains(tunnel.phase) else { return }
            self.close(tunnel, reason: "handshake_timeout")
        }
    }

    // Same TLS 1.3 + leaf-certificate pin as RemoteLink.connect.
    private static func parameters(pin: Data) -> NWParameters {
        let tls = NWProtocolTLS.Options()
        sec_protocol_options_set_min_tls_protocol_version(tls.securityProtocolOptions, .TLSv13)
        sec_protocol_options_set_max_tls_protocol_version(tls.securityProtocolOptions, .TLSv13)
        sec_protocol_options_set_verify_block(tls.securityProtocolOptions, { _, trust, complete in
            let object = sec_trust_copy_ref(trust).takeRetainedValue()
            guard let certificates = SecTrustCopyCertificateChain(object) as? [SecCertificate], let leaf = certificates.first else { complete(false); return }
            let digest = Data(SHA256.hash(data: SecCertificateCopyData(leaf) as Data))
            var difference: UInt8 = 0
            for i in 0..<32 { difference |= digest[i] ^ pin[i] }
            complete(difference == 0)
        }, DispatchQueue(label: "dev.iphoneuse.selfrunner.tunnel.tls"))
        return NWParameters(tls: tls, tcp: NWProtocolTCP.Options())
    }

    // Length-prefixed JSON frames until "open"; raw bytes after "opened".
    private func receiveFrames(_ tunnel: Tunnel, token: Int) {
        tunnel.outer.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, end, error in
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
            tunnel.phase = .idle
            dialing = false; backoff = 0
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
                        self.pump(from: tunnel.outer, to: local, tunnel: tunnel, token: token)
                    } else {
                        local.send(content: early, completion: .contentProcessed { [weak self] error in
                            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                            if error != nil { self.close(tunnel, reason: "local_send"); return }
                            self.pump(from: tunnel.outer, to: local, tunnel: tunnel, token: token)
                        })
                    }
                    self.pump(from: local, to: tunnel.outer, tunnel: tunnel, token: token)
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

    // One direction: receive -> send -> wait for completion -> receive, so a slow side throttles the
    // other. The Mac side's EOF becomes a TCP half-close towards the runner. The runner's EOF ends the
    // tunnel: Network.framework does not half-close a TLS connection on a final send, so the last
    // bytes go out and then the whole tunnel is cancelled gracefully (close_notify + FIN).
    private func pump(from: NWConnection, to: NWConnection, tunnel: Tunnel, token: Int) {
        from.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, end, error in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            if error != nil { self.close(tunnel, reason: "pipe_error"); return }
            let payload = (data?.isEmpty ?? true) ? nil : data
            if end {
                let toRunner = to !== tunnel.outer
                to.send(content: payload, contentContext: toRunner ? .finalMessage : .defaultMessage, isComplete: toRunner,
                         completion: .contentProcessed { [weak self] _ in
                    guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                    if !toRunner { self.close(tunnel, reason: "done") }
                })
                return
            }
            guard let payload else { self.pump(from: from, to: to, tunnel: tunnel, token: token); return }
            to.send(content: payload, completion: .contentProcessed { [weak self] error in
                guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
                if error != nil { self.close(tunnel, reason: "pipe_error"); return }
                self.pump(from: from, to: to, tunnel: tunnel, token: token)
            })
        }
    }

    private func sendFrame(_ frame: [String: Any], tunnel: Tunnel, then: (() -> Void)? = nil) {
        guard let data = try? JSONSerialization.data(withJSONObject: frame), data.count <= Self.preAuthLimit else {
            close(tunnel, reason: "frame"); return
        }
        let n = UInt32(data.count)
        let header = Data([UInt8((n >> 24) & 255), UInt8((n >> 16) & 255), UInt8((n >> 8) & 255), UInt8(n & 255)])
        let token = epoch
        tunnel.outer.send(content: header + data, completion: .contentProcessed { [weak self] error in
            guard let self, token == self.epoch, self.tunnels[tunnel.id] === tunnel else { return }
            if error != nil { self.close(tunnel, reason: "send"); return }
            then?()
        })
    }

    private func close(_ tunnel: Tunnel, reason: String) {
        guard tunnels[tunnel.id] === tunnel else { return }
        tunnels[tunnel.id] = nil
        let beforeIdle = [.connecting, .challenge, .welcome].contains(tunnel.phase)
        tunnel.local?.cancel(); tunnel.outer.cancel()
        if beforeIdle {
            // Authentication or reachability failure: back off. Normal recycling (the gateway's idle
            // timeout, a finished stream) refills at the plain dial spacing.
            dialing = false
            backoff = min(30, max(1, backoff * 2))
            log("tunnel \(tunnel.id): \(reason), retry in \(Int(backoff))s")
        } else if reason != "done" && reason != "idle_closed" {
            log("tunnel \(tunnel.id): closed (\(reason))")
        }
        fill()
    }
}
