import Foundation

// Protocol probe only: request a handshake, never attempt pair-setup or pair-verify.
// Wire reference: idevice 41c388b, remote_pairing/{mod,socket}.rs (MIT).
enum RemotePairingHandshake {
    static let magic = Data("RPPairing".utf8)
    static func request() throws -> Data {
        let body: [String: Any] = [
            "message": ["plain": ["_0": ["request": ["_0": ["handshake": ["_0": [
                "hostOptions": ["attemptPairVerify": true], "wireProtocolVersion": 19
            ]]]]]]], "originatedBy": "host", "sequenceNumber": 0
        ]
        let json = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        guard json.count <= UInt16.max else { throw Failure.invalidFrame }
        var frame = magic
        frame.append(UInt8(json.count >> 8))
        frame.append(UInt8(json.count & 255))
        frame.append(json)
        return frame
    }
    enum Failure: Error { case invalidFrame, unexpectedResponse }
    /// Returns nil for an incomplete frame. Reports no device identifiers or keys.
    static func response(_ bytes: Data) throws -> String? {
        let bytes = [UInt8](bytes)
        guard bytes.count >= magic.count + 2 else { return nil }
        guard Array(bytes.prefix(magic.count)) == Array(magic) else { throw Failure.invalidFrame }
        let offset = magic.count + 2
        let length = Int(bytes[magic.count]) << 8 | Int(bytes[magic.count + 1])
        guard length > 0 else { throw Failure.invalidFrame }
        guard bytes.count >= offset + length else { return nil }
        let object = try JSONSerialization.jsonObject(with: Data(bytes[offset..<offset+length]))
        guard let envelope = object as? [String: Any],
              envelope["originatedBy"] as? String == "device",
              let message = envelope["message"] as? [String: Any],
              let plain = message["plain"] as? [String: Any],
              let value = plain["_0"] as? [String: Any],
              let response = value["response"] as? [String: Any],
              let content = response["_1"] as? [String: Any],
              let handshake = content["handshake"] as? [String: Any],
              let details = handshake["_0"] as? [String: Any]
        else { throw Failure.unexpectedResponse }
        let version = (details["wireProtocolVersion"] as? NSNumber)?.stringValue ?? "unspecified"
        return "RemotePairing handshake received; wire protocol \(version); pairing and authorization not attempted"
    }
}
