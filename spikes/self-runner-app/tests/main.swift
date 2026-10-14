import Foundation
import CryptoKit
import Network
import Darwin
setbuf(stdout, nil)

func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }
func keyData(_ key: SymmetricKey) -> Data { key.withUnsafeBytes { Data($0) } }
func check(_ name: String, _ data: Data, _ expected: String) {
    guard hex(data) == expected else { fatalError("\(name) mismatch") }
    print("\(name) ok")
}
let psk = Data(0..<32)
let client = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data(32..<64))
let server = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data(64..<96))
let m1 = RemoteE2E.message(client), m2 = RemoteE2E.message(server, server: true)
check("m1", m1, "5352453101358072d6365880d1aeea329adf9121383851ed21a28e3b75e965d0d2cd166254")
check("m2", m2, "535245310279a631eede1bf9c98f12032cdeadd0e7a079398fc786b88cc846ec89af85a51a")
let keys = try RemoteE2E.derive(psk: psk, privateKey: client, m1: m1, m2: m2)
check("dh", keys.0, "04c304fb1ca83cee75e206344231f33797e07d9929db670994b7c6fbeb1dc255")
check("transcript_hash", keys.1, "418f81a9f94e6c194b81d5f083462fc6ecfd3c94df7f91bc14f055c14b48b219")
check("key_c2s", keyData(keys.2), "840004597a499caeca91c295843319f40c52774522be214b5d6571e378be3c5f")
check("key_s2c", keyData(keys.3), "c79a8bdb55636d741b168d40fe70b9afdc4e22f9933454f26f09e21f0c04ae62")
let hello = try RemoteE2E.seal(Data([0]) + Data("hello".utf8), key: keys.3, seq: 0)
let world = try RemoteE2E.seal(Data([0]) + Data("world".utf8), key: keys.2, seq: 0)
let eof = try RemoteE2E.seal(Data([1]), key: keys.2, seq: 1)
check("s2c DATA", hello, "b80ae39be1e326570554cdbcd547fbc28382c664b5d6")
check("c2s DATA", world, "cd03d4ee6288919dea0bf25a474d3d5432331ef75b35")
check("c2s EOF", eof, "6fc3dc760a5c5ef263404a0f4436124ca8")
var fake: [String: Any] = ["v": 1, "host": "localhost", "port": 7381, "cert_sha256": String(repeating: "0", count: 64), "psk": psk.base64EncodedString(), "device_id": String(repeating: "0", count: 32)]
func pair(_ url: String?) throws -> RemotePairing {
    var json = fake; json["ws_url"] = url
    return try RemotePairing.validate(JSONSerialization.data(withJSONObject: json))
}
let legacy = try pair(nil)
guard legacy.ws_url == nil, legacy.route == "e285a22ef3cf3f7b6c09bb2188bc75ee" else { fatalError("route/legacy") }
print("route ok")
for url in ["wss://phone.example.com", "wss://example.com:443/", "ws://127.0.0.1:7383", "ws://localhost", "wss://a_b:1"] {
    let p = try pair(url)
    guard p.webSocketURL?.path == "/selfrunner/v1/" + p.route else { fatalError("full URL") }
}
for url in ["ws://example.com", "http://localhost", "wss://x/a", "wss://x//", "wss://x?", "wss://x#", "wss://u@x", "wss://u:p@x", "wss://x:0", "wss://x:65536", "wss://x:", "wss://", "wss://x%20y", "wss://x\\y", "wss://" + String(repeating: "a", count: 254), String(repeating: "x", count: 513)] {
    do { _ = try pair(url); fatalError("accepted invalid ws_url") } catch {}
}
print("ws_url validation ok")
func rejects(_ body: () throws -> Void) {
    do { try body(); fatalError("expected rejection") } catch {}
}
guard try RemoteE2E.open(hello, key: keys.3, seq: 0) == Data([0]) + Data("hello".utf8) else { fatalError("decrypt") }
rejects { _ = try RemoteE2E.open(world, key: keys.2, seq: 1) }
rejects { _ = try RemoteE2E.open(world, key: keys.3, seq: 0) }
var corrupt = world; corrupt[corrupt.startIndex] ^= 1
rejects { _ = try RemoteE2E.open(corrupt, key: keys.2, seq: 0) }
rejects { _ = try RemoteE2E.seal(Data([1]), key: keys.2, seq: .max) }
rejects { _ = try RemoteE2E.open(RemoteE2E.seal(Data([0]), key: keys.2, seq: 0), key: keys.2, seq: 0) }
rejects { _ = try RemoteE2E.derive(psk: psk, privateKey: client, m1: m1, m2: Data("SRE1".utf8) + Data([2]) + Data(repeating: 0, count: 32)) }
print("record rejection ok")
if ProcessInfo.processInfo.environment["GATEWAY_WS_URL"] == nil || ProcessInfo.processInfo.environment["PAIRING_FILE"] == nil {
    print("gateway integration skipped (GATEWAY_WS_URL and PAIRING_FILE unset)")
} else {
    try runIntegration()
}

// Explicit opt-in only: PAIRING_FILE must be a disposable test fixture, not a real pairing.
final class ControlProbe {
    let transport: RemoteTransport
    let pair: RemotePairing
    var buffer = Data()
    var welcomed = false
    var pongWritten = false
    var onPong: (() -> Void)?
    init(_ pair: RemotePairing) { self.pair = pair; transport = WebSocketRemoteTransport(pair) }
    func start() {
        transport.failed = { _ in fatalError("integration control transport failed") }
        transport.ready = { [weak self] in self?.read() }
        transport.start()
    }
    func send(_ frame: [String: Any], done: (() -> Void)? = nil) {
        let data = try! JSONSerialization.data(withJSONObject: frame)
        let n = UInt32(data.count)
        let header = Data([UInt8(truncatingIfNeeded: n >> 24), UInt8(truncatingIfNeeded: n >> 16), UInt8(truncatingIfNeeded: n >> 8), UInt8(truncatingIfNeeded: n)])
        transport.send(header + data) { error in
            guard error == nil else { fatalError("integration control send failed") }
            done?()
        }
    }
    func read() {
        transport.receive { [weak self] data, end, error in
            guard let self else { return }
            guard error == nil, !end else { fatalError("integration control ended") }
            if let data { self.buffer.append(data) }
            while self.buffer.count >= 4 {
                let n = self.buffer.prefix(4).reduce(0) { ($0 << 8) | Int($1) }
                guard n > 0, n <= 4096 else { fatalError("integration frame length") }
                if self.buffer.count < n + 4 { break }
                let frame = try! JSONSerialization.jsonObject(with: self.buffer.subdata(in: 4..<(n + 4))) as! [String: Any]
                self.buffer = Data(self.buffer.dropFirst(n + 4))
                switch frame["t"] as? String {
                case "challenge":
                    guard let nonce = frame["nonce"] as? String, let bytes = Data(base64Encoded: nonce), bytes.count == 32 else { fatalError("challenge") }
                    let message = Data("selfrunner-v1".utf8) + bytes + Data(self.pair.device_id.utf8)
                    let mac = Data(HMAC<SHA256>.authenticationCode(for: message, using: SymmetricKey(data: self.pair.key)))
                    self.send(["t": "hello", "v": 1, "device_id": self.pair.device_id, "mac": mac.base64EncodedString(), "app_version": "transport-check", "session": ["ready": true]])
                case "welcome": self.welcomed = true; print("gateway control welcome ok")
                case "ping":
                    guard self.welcomed else { fatalError("ping before welcome") }
                    self.send(["t": "pong", "seq": frame["seq"] ?? NSNull()]) {
                        if !self.pongWritten { self.pongWritten = true; print("gateway control ping/pong ok"); self.onPong?() }
                    }
                default: fatalError("unexpected control frame")
                }
            }
            self.read()
        }
    }
}

func runIntegration() throws {
    let env = ProcessInfo.processInfo.environment
    let file = URL(fileURLWithPath: env["PAIRING_FILE"]!)
    // This harness enforces the published fake PSK; it never imports into Keychain.
    var pair = try RemotePairing.validate(Data(contentsOf: file))
    guard pair.key == psk else { fatalError("PAIRING_FILE must use the vector fake PSK") }
    pair.ws_url = env["GATEWAY_WS_URL"]!
    guard RemotePairing.validWebSocketBase(pair.ws_url!) else { fatalError("invalid GATEWAY_WS_URL") }
    let forwardPort = UInt16(env["GATEWAY_FORWARD_PORT"] ?? "18100")!
    let listener = try NWListener(using: .tcp, on: .any)
    let tunnel = RemoteTunnel()
    let control = ControlProbe(pair)
    var echoConnections: [NWConnection] = []
    var forward: NWConnection?
    var startedForward = false
    var roundTrip = false
    var sawEOF = false
    let payload = Data((0..<(1024 * 1024 + 123)).map { UInt8(truncatingIfNeeded: $0) })
    var received = Data()
    func echo(_ c: NWConnection) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, end, error in
            guard error == nil else { fatalError("echo receive failed") }
            c.send(content: data, contentContext: end ? .finalMessage : .defaultMessage, isComplete: true,
                   completion: .contentProcessed { error in
                guard error == nil else { fatalError("echo send failed") }
                if !end { echo(c) }
            })
        }
    }
    func readForward(_ c: NWConnection) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, end, error in
            guard error == nil else { fatalError("forward receive failed") }
            if let data { received.append(data) }
            guard received.count <= payload.count else { fatalError("extra echo bytes") }
            if end {
                guard received == payload else { fatalError("echo mismatch") }
                roundTrip = true; sawEOF = true; print("gateway tunnel open/opened, >1MB echo and half-close ok")
            } else { readForward(c) }
        }
    }
    listener.newConnectionHandler = { c in
        echoConnections.append(c)
        c.stateUpdateHandler = { state in if case .ready = state { echo(c) } }
        c.start(queue: .main)
    }
    listener.stateUpdateHandler = { state in
        switch state {
        case .ready:
            tunnel.ports = [8100: listener.port!.rawValue]
            tunnel.configure(enabled: true, pairing: pair)
            control.start()
        case .failed: fatalError("echo listener failed")
        default: break
        }
    }
    listener.start(queue: .main)
    let deadline = Date().addingTimeInterval(55)
    while Date() < deadline && !(roundTrip && sawEOF && control.pongWritten) {
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        // FORWARD_EXTERNAL=<秒>: keep the tunnel pool up and let an external client drive the forward port.
        if let hold = env["FORWARD_EXTERNAL"].flatMap(Double.init), control.welcomed && !startedForward {
            startedForward = true; print("holding tunnels for external forward client")
            let until = Date().addingTimeInterval(hold)
            while Date() < until { RunLoop.main.run(until: Date().addingTimeInterval(0.05)) }
            roundTrip = true; sawEOF = true
        }
        if control.welcomed && !startedForward {
            startedForward = true
            let c = NWConnection(host: "127.0.0.1", port: NWEndpoint.Port(rawValue: forwardPort)!, using: .tcp)
            forward = c
            c.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    c.send(content: payload, contentContext: .finalMessage, isComplete: true,
                           completion: .contentProcessed { error in guard error == nil else { fatalError("forward send failed") } })
                    readForward(c)
                case .failed(let error):
                    // After both FINs macOS may report the socket as failed (ENETDOWN) before delivering the
                    // final isComplete receive; the gateway log still records EOF in both directions.
                    if received == payload { roundTrip = true; sawEOF = true; print("gateway tunnel open/opened, >1MB echo ok (close reported as \(error))") }
                    else { fatalError("gateway forward connect failed: \(error) received=\(received.count)") }
                default: break
                }
            }
            c.start(queue: .main)
        }
    }
    guard roundTrip, sawEOF, control.pongWritten else { fatalError("gateway integration timeout") }
    control.transport.cancel(); tunnel.configure(enabled: false, pairing: nil)
    forward?.cancel(); listener.cancel(); echoConnections.forEach { $0.cancel() }
}
