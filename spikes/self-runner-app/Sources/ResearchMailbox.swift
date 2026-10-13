import Foundation

// Research-only USB file protocol. No network listener or arbitrary code/path execution.
struct ResearchCommand: Decodable {
    let version: Int
    let id: UUID
    let runID: UUID
    let expiresAt: Double
    let action: String

    static let actions: Set<String> = ["start", "e0", "e1", "e1b", "e1c", "read", "read-runner", "read-delayed", "screenshot", "tap", "ddi-mount", "ddi-ticket", "ddi-probe", "tap-offer", "remote-status"]

    static func decode(_ data: Data, runID: UUID, seen: Set<UUID>, now: Double) throws -> ResearchCommand {
        guard data.count <= 4096 else { throw MailboxError.invalid("command exceeds 4096 bytes") }
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(object.keys) == Set(["version", "id", "runID", "expiresAt", "action"]) else {
            throw MailboxError.invalid("unexpected command fields")
        }
        let command = try JSONDecoder().decode(Self.self, from: data)
        guard command.version == 1, command.runID == runID,
              command.expiresAt.isFinite, command.expiresAt > now,
              command.expiresAt <= now + 120,
              actions.contains(command.action), !seen.contains(command.id) else {
            throw MailboxError.invalid("version/runID/expiry/action/replay validation failed")
        }
        return command
    }
}

enum MailboxError: Error { case invalid(String) }

final class ResearchMailbox {
    let runID = UUID()
    private let documents: URL
    private var seen = Set<UUID>()
    private var lastData: Data?
    private var current: ResearchCommand?
    private var phase = "idle"
    private var result = ""
    private var rejection: [String: String]?

    init(documents: URL) { self.documents = documents }

    // Called on main. A rejected command never displaces an in-flight command.
    func poll(state: [String: Any], execute: (ResearchCommand) -> Void) {
        let url = documents.appendingPathComponent("research-command.json")
        // Bound allocation before parsing, including a file still being transferred over USB.
        if let file = try? FileHandle(forReadingFrom: url) {
            defer { try? file.close() }
            if let data = try? file.read(upToCount: 4097), data != lastData {
                lastData = data
                do {
                    let command = try ResearchCommand.decode(data, runID: runID, seen: seen, now: Date().timeIntervalSince1970)
                    guard seen.count < 128 else { throw MailboxError.invalid("128-command limit; relaunch required") }
                    seen.insert(command.id)
                    guard phase != "running" && phase != "stillRunning" else { throw MailboxError.invalid("previous command still running") }
                    current = command; phase = "running"; result = ""; rejection = nil
                    execute(command)
                } catch {
                    let id = (try? JSONDecoder().decode(ResearchCommand.self, from: data))?.id.uuidString ?? "unknown"
                    rejection = ["id": id, "error": String(describing: error)]
                }
            }
        }
        publish(state: state)
    }

    func finish(_ id: UUID?, result: String, rejected: Bool = false) {
        guard let id, current?.id == id else { return }
        phase = rejected ? "rejected" : "returned" // Returned is not a claim of operation success.
        self.result = result
    }

    func watchdog(_ id: UUID?) {
        guard let id, current?.id == id, phase == "running" else { return }
        phase = "stillRunning"
    }

    private func publish(state: [String: Any]) {
        var status: [String: Any] = ["version": 1, "runID": runID.uuidString,
            "updatedAt": Date().timeIntervalSince1970, "phase": phase, "result": result, "state": state]
        if let command = current { status["id"] = command.id.uuidString; status["action"] = command.action }
        if let rejection { status["rejection"] = rejection }
        do {
            let data = try JSONSerialization.data(withJSONObject: status, options: [.sortedKeys])
            try data.write(to: documents.appendingPathComponent("research-status.json"), options: .atomic)
        } catch { NSLog("research mailbox status write failed: %@", String(describing: error)) }
    }
}
