import Foundation

enum AgentTaskList {
    private struct Envelope: Decodable {
        var ok: Bool?
        var tasks: [AgentTaskSnapshot]
    }

    static func decode(_ data: Data) throws -> [AgentTaskSnapshot] {
        let decoder = JSONDecoder()
        if let bare = try? decoder.decode([AgentTaskSnapshot].self, from: data) { return bare }
        let envelope = try decoder.decode(Envelope.self, from: data)
        guard envelope.ok != false else { throw URLError(.cannotParseResponse) }
        return envelope.tasks
    }
}
