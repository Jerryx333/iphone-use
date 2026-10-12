import Foundation

enum FlowSavePayload {
    static func encode(id: String, name: String, description: String?, flow: Data?) throws -> Data {
        var body: [String: Any] = ["id": id, "name": name]
        if let description, !description.isEmpty { body["description"] = description }
        if let flow {
            // Supplied drafts must never silently fall back to another trail.
            guard let object = try JSONSerialization.jsonObject(with: flow) as? [String: Any] else {
                throw URLError(.cannotParseResponse)
            }
            body["flow"] = object
        }
        return try JSONSerialization.data(withJSONObject: body)
    }

    static func savedID(from data: Data, expectedID: String) throws -> String {
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              json["ok"] as? Bool == true,
              let saved = json["saved"] as? [String: Any],
              let id = saved["id"] as? String, id == expectedID else {
            throw URLError(.cannotParseResponse)
        }
        return id
    }
}
