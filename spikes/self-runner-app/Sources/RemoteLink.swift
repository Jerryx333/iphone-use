import Foundation
import Network
import Security
import CryptoKit

// All transport state lives on main; action workers never own a connection.
final class RemoteLink {
    private var connection: NWConnection?
    private let monitor = NWPathMonitor()
    private var pairing: RemotePairing?
    private var enabled = false
    private var epoch = 0
    private var retry: DispatchWorkItem?
    private var backoff = 1.0
    private var phase = "offline"
    private var buffer = Data()
    private var lastData = Date()
    private var heartbeat: Timer?
    var ready: () -> Bool = { false }
    var request: ([String: Any], @escaping ([String: Any]) -> Void) -> Void = { _, _ in }
    var status: String { phase }
    init() {
        monitor.pathUpdateHandler = { [weak self] _ in DispatchQueue.main.async { self?.reconnectNow() } }
        monitor.start(queue: DispatchQueue(label: "dev.iphoneuse.selfrunner.path"))
    }
    func configure(enabled: Bool, pairing: RemotePairing?) {
        self.enabled = enabled; self.pairing = pairing
        disconnect(); backoff = 1
        reconnectNow()
    }
    func reconnectNow() {
        guard enabled, pairing != nil, connection == nil else { return }
        retry?.cancel(); retry = nil
        connect()
    }
    private func disconnect() {
        epoch += 1
        connection?.cancel(); connection = nil
        heartbeat?.invalidate(); heartbeat = nil
        retry?.cancel(); retry = nil
        buffer.removeAll(); phase = "offline"
    }
    private func failed(_ token: Int) {
        guard token == epoch else { return }
        disconnect()
        guard enabled, pairing != nil else { return }
        let item = DispatchWorkItem { [weak self] in self?.reconnectNow() }
        retry = item
        DispatchQueue.main.asyncAfter(deadline: .now() + backoff, execute: item)
        backoff = min(30, backoff * 2)
    }
    private func connect() {
        guard let pair = pairing else { return }
        epoch += 1; let token = epoch
        let tls = NWProtocolTLS.Options()
        sec_protocol_options_set_min_tls_protocol_version(tls.securityProtocolOptions, .TLSv13)
        sec_protocol_options_set_max_tls_protocol_version(tls.securityProtocolOptions, .TLSv13)
        let pin = pair.pin
        sec_protocol_options_set_verify_block(tls.securityProtocolOptions, { _, trust, complete in
            let object = sec_trust_copy_ref(trust).takeRetainedValue()
            guard let certificates = SecTrustCopyCertificateChain(object) as? [SecCertificate], let leaf = certificates.first else { complete(false); return }
            let digest = Data(SHA256.hash(data: SecCertificateCopyData(leaf) as Data))
            var difference: UInt8 = 0
            for i in 0..<32 { difference |= digest[i] ^ pin[i] }
            complete(difference == 0)
        }, DispatchQueue(label: "dev.iphoneuse.selfrunner.tls"))
        let c = NWConnection(host: NWEndpoint.Host(pair.host), port: NWEndpoint.Port(rawValue: pair.port)!,
                             using: NWParameters(tls: tls, tcp: NWProtocolTCP.Options()))
        connection = c; phase = "connecting"
        c.stateUpdateHandler = { [weak self] state in
            guard let self, token == self.epoch else { return }
            switch state {
            case .ready: self.phase = "challenge"; self.receive(c, token: token)
            case .failed, .cancelled, .waiting: self.failed(token)
            default: break
            }
        }
        c.start(queue: .main)
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) { [weak self] in
            guard let self, token == self.epoch, self.phase != "online" else { return }
            self.failed(token)
        }
    }
    private func receive(_ c: NWConnection, token: Int) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, end, error in
            guard let self, token == self.epoch else { return }
            if let data { self.buffer.append(data) }
            while self.buffer.count >= 4 {
                let length = self.buffer.prefix(4).reduce(0) { ($0 << 8) | Int($1) }
                let limit = self.phase == "online" ? 16 * 1024 * 1024 : 4096
                guard length > 0, length <= limit else { self.failed(token); return }
                guard self.buffer.count >= length + 4 else { break }
                let bytes = self.buffer.subdata(in: 4..<(length + 4))
                self.buffer = Data(self.buffer.dropFirst(length + 4))
                guard let frame = try? JSONSerialization.jsonObject(with: bytes) as? [String: Any], self.handle(frame, token: token) else { self.failed(token); return }
                self.lastData = Date()
            }
            if end || error != nil { self.failed(token); return }
            self.receive(c, token: token)
        }
    }
    private func handle(_ frame: [String: Any], token: Int) -> Bool {
        let t = frame["t"] as? String
        if phase == "challenge" {
            guard t == "challenge", let nonce = frame["nonce"] as? String,
                  let data = Data(base64Encoded: nonce), data.count == 32, let pair = pairing else { return false }
            let message = Data("selfrunner-v1".utf8) + data + Data(pair.device_id.utf8)
            let mac = Data(HMAC<SHA256>.authenticationCode(for: message, using: SymmetricKey(data: pair.key)))
            phase = "welcome"
            send(["t": "hello", "v": 1, "device_id": pair.device_id, "mac": mac.base64EncodedString(),
                  "app_version": Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown",
                  "session": ["ready": ready()]], token: token)
            return true
        }
        if phase == "welcome" {
            guard t == "welcome" else { return false }
            phase = "online"; backoff = 1; lastData = Date()
            heartbeat = Timer.scheduledTimer(withTimeInterval: 15, repeats: true) { [weak self] _ in
                guard let self, token == self.epoch else { return }
                if Date().timeIntervalSince(self.lastData) >= 45 { self.failed(token) }
            }
            sessionChanged(); return true
        }
        guard phase == "online" else { return false }
        switch t {
        case "ping": send(["t": "pong", "seq": frame["seq"] ?? NSNull()], token: token)
        case "pong": break
        case "req": request(frame) { [weak self] response in self?.send(response, token: token) }
        default: return false
        }
        return true
    }
    func sessionChanged() {
        if phase == "online" { send(["t": "event", "kind": "session", "ready": ready()], token: epoch) }
    }
    private func send(_ frame: [String: Any], token: Int) {
        guard token == epoch, let c = connection, let data = try? JSONSerialization.data(withJSONObject: frame) else { return }
        guard data.count <= (phase == "online" ? 16 * 1024 * 1024 : 4096) else { failed(token); return }
        let n = UInt32(data.count)
        let header = Data([UInt8((n >> 24) & 255), UInt8((n >> 16) & 255), UInt8((n >> 8) & 255), UInt8(n & 255)])
        c.send(content: header + data, completion: .contentProcessed { [weak self] error in
            if error != nil { self?.failed(token) }
        })
    }
}
