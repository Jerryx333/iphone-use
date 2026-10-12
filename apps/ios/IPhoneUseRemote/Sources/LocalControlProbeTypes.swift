import Foundation

// The app decodes diagnostic metadata; only the extension performs probes.
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
