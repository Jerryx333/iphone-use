import Foundation
import Network

struct Endpoint: Codable {
    let host: String
    let port: UInt16
    let purpose: String
}
struct ResultRow: Codable, Identifiable {
    var id: String { "\(endpoint.host):\(endpoint.port)" }
    let endpoint: Endpoint
    let outcome: String
    let detail: String
    let elapsedMilliseconds: Int
}
/// Socket and unauthenticated protocol probes. Never pairs, launches a runner, or reads credentials.
final class TCPAttempt {
    private let queue = DispatchQueue(label: "ipu-selfhost-probe")
    private var connection: NWConnection?
    private var finished = false
    private var received = Data()
    private let endpoint: Endpoint
    private let started = Date()
    private var continuation: CheckedContinuation<ResultRow, Never>?
    init(_ endpoint: Endpoint) { self.endpoint = endpoint }

    func run() async -> ResultRow {
        await withCheckedContinuation { continuation in
            queue.async {
                self.continuation = continuation
                let connection = NWConnection(
                    host: NWEndpoint.Host(self.endpoint.host),
                    port: NWEndpoint.Port(rawValue: self.endpoint.port)!, using: .tcp)
                self.connection = connection
                connection.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        if self.endpoint.port == 49152 { self.handshake(connection) }
                        else { self.finish("tcp_open", "TCP connected; protocol and authorization not verified") }
                    case .failed(let error): self.finish("tcp_failed", String(describing: error))
                    case .waiting(let error):
                        // Waiting can recover after a permission prompt; allow the deadline.
                        self.waitingDetail = String(describing: error)
                        if case .posix(let code) = error, code == .EPERM || code == .EACCES {
                            self.finish("tcp_denied", String(describing: error))
                        }
                    default: break
                    }
                }
                connection.start(queue: self.queue)
                self.queue.asyncAfter(deadline: .now() + 5) {
                    self.finish("probe_timeout", self.waitingDetail ?? "No completed probe within 5 seconds")
                }
            }
        }
    }
    private func handshake(_ connection: NWConnection) {
        do {
            let frame = try RemotePairingHandshake.request()
            connection.send(content: frame, completion: .contentProcessed { error in
                if let error { self.finish("handshake_send_failed", String(describing: error)) }
                else { self.receiveHandshake(connection) }
            })
        } catch { finish("handshake_invalid_request", "Could not encode protocol probe") }
    }
    private func receiveHandshake(_ connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65546) { bytes, _, complete, error in
            guard !self.finished else { return }
            if let bytes { self.received.append(bytes) }
            do {
                if let detail = try RemotePairingHandshake.response(self.received) {
                    self.finish("remote_pairing_handshake", detail)
                    return
                }
            } catch {
                self.finish("handshake_unrecognized", "TCP open, but response did not match RemotePairing handshake")
                return
            }
            if let error { self.finish("handshake_receive_failed", String(describing: error)) }
            else if complete { self.finish("handshake_closed", "Peer closed before a complete RemotePairing handshake") }
            else { self.receiveHandshake(connection) }
        }
    }
    private var waitingDetail: String?
    private func finish(_ outcome: String, _ detail: String) {
        guard !finished else { return }
        finished = true
        connection?.stateUpdateHandler = nil
        connection?.cancel()
        connection = nil
        continuation?.resume(returning: ResultRow(endpoint: endpoint, outcome: outcome,
            detail: detail, elapsedMilliseconds: Int(Date().timeIntervalSince(started) * 1000)))
        continuation = nil
    }
}
