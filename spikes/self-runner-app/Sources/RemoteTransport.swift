import Foundation
import Network
import Security
import CryptoKit

// All methods and callbacks run on main. Receive is demand driven; at most one read is outstanding.
protocol RemoteTransport: AnyObject {
    var ready: (() -> Void)? { get set }
    var failed: ((Error) -> Void)? { get set }
    var route: String { get }
    func start()
    func receive(_ completion: @escaping (Data?, Bool, Error?) -> Void)
    func send(_ data: Data, completion: @escaping (Error?) -> Void)
    func finish(completion: @escaping (Error?) -> Void)
    func cancel()
}

enum RemoteTransportError: Error { case protocolViolation, closed, overflow }

// Each owner has its own path history, including the LAN failure preference.
final class RemoteRoutePolicy {
    private let monitor = NWPathMonitor()
    private var path: NWPath?
    private var lanFailed = false
    var changed: (() -> Void)?
    init() {
        monitor.pathUpdateHandler = { [weak self] next in
            DispatchQueue.main.async {
                guard let self else { return }
                if self.path != next {
                    self.path = next; self.lanFailed = false; self.changed?()
                }
            }
        }
        monitor.start(queue: DispatchQueue(label: "dev.iphoneuse.selfrunner.routes"))
    }
    deinit { monitor.cancel() }
    func failure(_ route: String) { if route == "lan" { lanFailed = true } }
    /// A new pairing may point at a different LAN host; forget earlier failures.
    func reset() { lanFailed = false }
    func routes(_ pair: RemotePairing) -> [String] {
        guard pair.ws_url != nil else { return ["lan"] }
        return (path ?? monitor.currentPath).usesInterfaceType(.wifi) && !lanFailed ? ["lan", "wss"] : ["wss", "lan"]
    }
    static func make(_ pair: RemotePairing, route: String) -> RemoteTransport {
        route == "wss" ? WebSocketRemoteTransport(pair) : TLSRemoteTransport(pair)
    }
}

final class TLSRemoteTransport: RemoteTransport {
    var ready: (() -> Void)?
    var failed: ((Error) -> Void)?
    let route = "lan"
    private let connection: NWConnection
    init(_ pair: RemotePairing) {
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
        connection = NWConnection(host: NWEndpoint.Host(pair.host), port: NWEndpoint.Port(rawValue: pair.port)!,
                                  using: NWParameters(tls: tls, tcp: NWProtocolTCP.Options()))
    }
    func start() {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready: self?.ready?()
            case .failed(let error), .waiting(let error): self?.failed?(error)
            case .cancelled: self?.failed?(RemoteTransportError.closed)
            default: break
            }
        }
        connection.start(queue: .main)
    }
    func receive(_ completion: @escaping (Data?, Bool, Error?) -> Void) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, end, error in completion(data, end, error) }
    }
    func send(_ data: Data, completion: @escaping (Error?) -> Void) {
        connection.send(content: data, completion: .contentProcessed { completion($0) })
    }
    // TLS cannot half-close: the tunnel owner retains its original full-close behavior.
    func finish(completion: @escaping (Error?) -> Void) { completion(nil) }
    func cancel() { connection.stateUpdateHandler = nil; connection.cancel() }
}

// Pure crypto functions are shared with the macOS vector checks.
enum RemoteE2E {
    static func message(_ key: Curve25519.KeyAgreement.PrivateKey, server: Bool = false) -> Data {
        Data("SRE1".utf8) + Data([server ? 2 : 1]) + key.publicKey.rawRepresentation
    }
    static func derive(psk: Data, privateKey: Curve25519.KeyAgreement.PrivateKey, m1: Data, m2: Data) throws -> (Data, Data, SymmetricKey, SymmetricKey) {
        guard m1.count == 37, m2.count == 37, m1.prefix(5) == Data("SRE1".utf8) + Data([1]),
              m2.prefix(5) == Data("SRE1".utf8) + Data([2]) else { throw RemoteTransportError.protocolViolation }
        let publicKey = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: m2.suffix(32))
        let secret = try privateKey.sharedSecretFromKeyAgreement(with: publicKey)
        let dh = secret.withUnsafeBytes { Data($0) }
        guard dh.contains(where: { $0 != 0 }) else { throw RemoteTransportError.protocolViolation }
        let th = Data(SHA256.hash(data: Data("selfrunner-e2e-v1".utf8) + m1 + m2))
        let prk = HKDF<SHA256>.extract(inputKeyMaterial: SymmetricKey(data: psk + dh), salt: th)
        let c = HKDF<SHA256>.expand(pseudoRandomKey: prk, info: Data("selfrunner-e2e-v1 c2s".utf8), outputByteCount: 32)
        let s = HKDF<SHA256>.expand(pseudoRandomKey: prk, info: Data("selfrunner-e2e-v1 s2c".utf8), outputByteCount: 32)
        return (dh, th, c, s)
    }
    static func nonce(_ seq: UInt64) throws -> ChaChaPoly.Nonce {
        guard seq != UInt64.max else { throw RemoteTransportError.overflow }
        return try ChaChaPoly.Nonce(data: Data(repeating: 0, count: 4) + Data((0..<8).map { UInt8(truncatingIfNeeded: seq >> (56 - $0 * 8)) }))
    }
    static func seal(_ plaintext: Data, key: SymmetricKey, seq: UInt64) throws -> Data {
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: nonce(seq))
        return box.ciphertext + box.tag
    }
    static func open(_ record: Data, key: SymmetricKey, seq: UInt64) throws -> Data {
        guard record.count >= 17, record.count <= 65536 + 17 else { throw RemoteTransportError.protocolViolation }
        let box = try ChaChaPoly.SealedBox(nonce: nonce(seq), ciphertext: record.dropLast(16), tag: record.suffix(16))
        let plain = try ChaChaPoly.open(box, using: key)
        guard (plain.first == 0 && plain.count >= 2 && plain.count <= 65537) || plain == Data([1]) else { throw RemoteTransportError.protocolViolation }
        return plain
    }
}

final class WebSocketRemoteTransport: RemoteTransport {
    var ready: (() -> Void)?
    var failed: ((Error) -> Void)?
    let route = "wss"
    private let connection: NWConnection
    private var psk: Data
    private var privateKey: Curve25519.KeyAgreement.PrivateKey? = .init()
    private var m1 = Data()
    private var sendKey: SymmetricKey?
    private var receiveKey: SymmetricKey?
    private var tx: UInt64 = 0, rx: UInt64 = 0
    private var sentEOF = false, receivedEOF = false, stopped = false
    private struct Write { let data: Data; let eof: Bool; let completion: (Error?) -> Void }
    private var writes: [Write] = []
    private var queuedBytes = 0
    private var writing = false
    init(_ pair: RemotePairing) {
        psk = pair.key
        let url = pair.webSocketURL!
        let options = NWProtocolWebSocket.Options()
        options.autoReplyPing = true; options.maximumMessageSize = 65536 + 17
        let parameters = NWParameters(tls: url.scheme == "wss" ? NWProtocolTLS.Options() : nil, tcp: NWProtocolTCP.Options())
        parameters.defaultProtocolStack.applicationProtocols.insert(options, at: 0)
        connection = NWConnection(to: .url(url), using: parameters)
    }
    func start() {
        dispatchPrecondition(condition: .onQueue(.main))
        connection.stateUpdateHandler = { [weak self] state in
            guard let self, !self.stopped else { return }
            switch state {
            case .ready:
                self.m1 = RemoteE2E.message(self.privateKey!)
                self.sendMessage(self.m1) { [weak self] error in
                    guard let self, !self.stopped else { return }
                    if let error { self.fail(error); return }
                    self.readMessage { [weak self] data, error in
                        guard let self, !self.stopped else { return }
                        do {
                            if let error { throw error }
                            let keys = try RemoteE2E.derive(psk: self.psk, privateKey: self.privateKey!, m1: self.m1, m2: data!)
                            self.privateKey = nil; self.psk = Data(); self.m1 = Data()
                            self.sendKey = keys.2; self.receiveKey = keys.3; self.ready?()
                        } catch { self.fail(error) }
                    }
                }
            case .failed(let error), .waiting(let error): self.fail(error)
            case .cancelled: self.fail(RemoteTransportError.closed)
            default: break
            }
        }
        connection.start(queue: .main)
    }
    private func readMessage(_ completion: @escaping (Data?, Error?) -> Void) {
        connection.receiveMessage { [weak self] data, context, complete, error in
            guard let self, !self.stopped else { return }
            if let error { completion(nil, error); return }
            let opcode = (context?.protocolMetadata(definition: NWProtocolWebSocket.definition) as? NWProtocolWebSocket.Metadata)?.opcode
            if opcode == .ping || opcode == .pong { self.readMessage(completion); return }
            guard opcode == .binary, complete, let data, data.count <= 65536 + 17 else {
                completion(nil, RemoteTransportError.protocolViolation); return
            }
            completion(data, nil)
        }
    }
    func receive(_ completion: @escaping (Data?, Bool, Error?) -> Void) {
        dispatchPrecondition(condition: .onQueue(.main))
        guard !stopped, !receivedEOF, let key = receiveKey else { completion(nil, false, RemoteTransportError.closed); return }
        readMessage { [weak self] data, error in
            guard let self, !self.stopped else { return }
            do {
                if let error { throw error }
                let plain = try RemoteE2E.open(data!, key: key, seq: self.rx); self.rx += 1
                if plain.first == 1 {
                    self.receivedEOF = true
                    completion(nil, true, nil)
                    self.closeIfFinished()
                    if !self.stopped { self.watchAfterEOF() }
                } else { completion(Data(plain.dropFirst()), false, nil) }
            } catch { completion(nil, false, error); self.fail(error) }
        }
    }
    // Keep processing WS control messages after the read half closes. Any further binary is invalid.
    private func watchAfterEOF() {
        readMessage { [weak self] _, error in self?.fail(error ?? RemoteTransportError.protocolViolation) }
    }
    private func sendMessage(_ data: Data, completion: @escaping (Error?) -> Void) {
        let context = NWConnection.ContentContext(identifier: "e2e", metadata: [NWProtocolWebSocket.Metadata(opcode: .binary)])
        connection.send(content: data, contentContext: context, isComplete: true, completion: .contentProcessed { completion($0) })
    }
    func send(_ data: Data, completion: @escaping (Error?) -> Void) { enqueue(data, eof: false, completion: completion) }
    func finish(completion: @escaping (Error?) -> Void) { enqueue(Data(), eof: true, completion: completion) }
    private func enqueue(_ data: Data, eof: Bool, completion: @escaping (Error?) -> Void) {
        dispatchPrecondition(condition: .onQueue(.main))
        guard !stopped, !sentEOF, !writes.contains(where: { $0.eof }), sendKey != nil,
              queuedBytes + data.count <= 32 * 1024 * 1024, writes.count < 1024 else { completion(RemoteTransportError.closed); return }
        if data.isEmpty && !eof { completion(nil); return }
        writes.append(Write(data: data, eof: eof, completion: completion)); queuedBytes += data.count
        drain()
    }
    private func drain() {
        guard !writing, !stopped, let write = writes.first else { return }
        writing = true
        writeChunk(write, offset: 0)
    }
    private func writeChunk(_ write: Write, offset: Int) {
        guard !stopped else { return }
        do {
            let end = min(offset + 65536, write.data.count)
            let plain = write.eof ? Data([1]) : Data([0]) + write.data.subdata(in: offset..<end)
            let record = try RemoteE2E.seal(plain, key: sendKey!, seq: tx); tx += 1
            sendMessage(record) { [weak self] error in
                guard let self, !self.stopped else { return }
                if let error { self.fail(error); return }
                if end < write.data.count { self.writeChunk(write, offset: end); return }
                self.writes.removeFirst(); self.queuedBytes -= write.data.count; self.writing = false
                if write.eof { self.sentEOF = true }
                write.completion(nil); self.closeIfFinished(); self.drain()
            }
        } catch { fail(error) }
    }
    private func closeIfFinished() { if sentEOF && receivedEOF { cancel() } }
    private func fail(_ error: Error) {
        guard !stopped else { return }
        let pending = writes; let callback = failed
        cancel()
        pending.forEach { $0.completion(error) }; callback?(error)
    }
    func cancel() {
        stopped = true; connection.stateUpdateHandler = nil; connection.cancel()
        privateKey = nil; psk = Data(); sendKey = nil; receiveKey = nil
        writes.removeAll(); queuedBytes = 0
    }
}
