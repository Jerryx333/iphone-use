import Foundation
import Network
import NetworkExtension

/// Authenticated loopback bridge for Rust sockets that must enter this provider's tunnel.
final class ScopedTunnelBridge {
    private weak var provider: NEPacketTunnelProvider?
    private let queue = DispatchQueue(label: "ipu-scoped-tunnel-bridge")
    private var listener: Network.NWListener?
    private var pipes: [UUID: TunnelPipe] = [:]
    private(set) var port: UInt16?
    let token: String
    init(provider: NEPacketTunnelProvider, token: String) { self.provider = provider; self.token = token }
    func start(completion: @escaping (Error?) -> Void) {
        do {
            let parameters = Network.NWParameters.tcp
            parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
            let listener = try Network.NWListener(using: parameters, on: .any)
            self.listener = listener
            var completed = false
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    guard !completed else { return }; completed = true
                    self.port = listener.port?.rawValue
                    completion(nil)
                case .failed(let error):
                    completed = true
                    completion(error)
                default: break
                }
            }
            listener.newConnectionHandler = { connection in
                guard self.pipes.count < 16, let provider = self.provider else { connection.cancel(); return }
                let id = UUID()
                let pipe = TunnelPipe(client: connection, provider: provider, token: self.token, queue: self.queue) {
                    self.pipes.removeValue(forKey: id)
                }
                self.pipes[id] = pipe
                pipe.start()
            }
            listener.start(queue: queue)
        } catch { completion(error) }
    }
    func stop() {
        queue.async {
            self.listener?.stateUpdateHandler = nil
            self.listener?.newConnectionHandler = nil
            self.listener?.cancel(); self.listener = nil
            let pipes = Array(self.pipes.values)
            pipes.forEach { $0.close() }
        }
    }
}

private final class TunnelPipe {
    private struct Request: Decodable { let port: UInt16; let token: String }
    private let client: Network.NWConnection
    private weak var provider: NEPacketTunnelProvider?
    private let token: String
    private let queue: DispatchQueue
    private let closed: () -> Void
    private var target: NWTCPConnection?
    private var observation: NSKeyValueObservation?
    private var header = Data()
    private var authorized = false
    private var connected = false
    private var finished = false
    init(client: Network.NWConnection, provider: NEPacketTunnelProvider, token: String,
         queue: DispatchQueue, closed: @escaping () -> Void) {
        self.client = client; self.provider = provider; self.token = token; self.queue = queue; self.closed = closed
    }
    func start() {
        client.stateUpdateHandler = { state in
            if case .failed = state { self.close() }
            if case .cancelled = state { self.close() }
        }
        client.start(queue: queue)
        readHeader()
        queue.asyncAfter(deadline: .now() + 5) { if !self.authorized { self.close() } }
    }
    private func readHeader() {
        client.receive(minimumIncompleteLength: 1, maximumLength: 512) { data, _, complete, error in
            guard !self.finished else { return }
            if let data { self.header.append(data) }
            guard error == nil, self.header.count <= 512 else { self.close(); return }
            if let newline = self.header.firstIndex(of: 10) {
                guard newline == self.header.count - 1,
                      let request = try? JSONDecoder().decode(Request.self, from: self.header.prefix(upTo: newline)),
                      request.token == self.token, request.port > 0, let provider = self.provider else { self.close(); return }
                self.authorized = true
                self.header.resetBytes(in: 0..<self.header.count); self.header = Data()
                let endpoint = NWHostEndpoint(hostname: "10.7.0.1", port: String(request.port))
                let target = provider.createTCPConnectionThroughTunnel(to: endpoint, enableTLS: false, tlsParameters: nil, delegate: nil)
                self.target = target
                self.observation = target.observe(\.state, options: [.new, .initial]) { connection, _ in
                    self.queue.async {
                        guard !self.finished else { return }
                        if connection.state == .connected && !self.connected {
                            self.connected = true
                            self.client.send(content: Data([1]), completion: .contentProcessed { error in
                                if error != nil { self.close(); return }
                                self.readClient(); self.readTarget()
                            })
                        } else if connection.state == .disconnected || connection.state == .cancelled { self.close() }
                    }
                }
                self.queue.asyncAfter(deadline: .now() + 8) { if !self.connected { self.close() } }
            } else if complete { self.close() }
            else { self.readHeader() }
        }
    }
    private func readClient() {
        client.receive(minimumIncompleteLength: 1, maximumLength: 32_768) { data, _, complete, error in
            guard !self.finished else { return }
            guard error == nil, let data, !data.isEmpty else { self.close(); return }
            self.target?.write(data) { error in
                self.queue.async {
                    if error != nil || complete { self.close() }
                    else { self.readClient() }
                }
            }
        }
    }
    private func readTarget() {
        target?.readMinimumLength(1, maximumLength: 32_768) { data, error in
            self.queue.async {
                guard !self.finished else { return }
                guard error == nil, let data, !data.isEmpty else { self.close(); return }
                self.client.send(content: data, completion: .contentProcessed { error in
                    if error != nil { self.close() }
                    else { self.readTarget() }
                })
            }
        }
    }
    func close() {
        guard !finished else { return }; finished = true
        observation?.invalidate(); observation = nil
        target?.cancel(); target = nil
        client.stateUpdateHandler = nil; client.cancel()
        closed()
    }
}
