import Foundation
import CryptoKit

let importKey = Curve25519.Signing.PrivateKey()
var importFixture: [String: Any] = ["private_key": importKey.rawRepresentation,
    "public_key": importKey.publicKey.rawRepresentation, "identifier": UUID().uuidString]
func importBytes(_ fixture: [String: Any]) throws -> Data {
    try PropertyListSerialization.data(fromPropertyList: fixture, format: .xml, options: 0)
}
try PairRecordImport.validate(importBytes(importFixture))
importFixture["public_key"] = Curve25519.Signing.PrivateKey().publicKey.rawRepresentation
do { try PairRecordImport.validate(importBytes(importFixture)); fatalError("Mismatched pairing keys accepted") }
catch { }
do { try PairRecordImport.validate(importBytes(["HostID": "synthetic-lockdown-record"])); fatalError("Legacy lockdown record accepted") }
catch { }
do { try PairRecordImport.validate(Data(repeating: 0, count: 65_537)); fatalError("Oversized record accepted") }
catch { }

func frame(_ object: Any) throws -> Data {
    let body = try JSONSerialization.data(withJSONObject: object)
    return Data("RPPairing".utf8) + Data([UInt8(body.count >> 8), UInt8(body.count & 255)]) + body
}
let fixture: [String: Any] = [
    "originatedBy": "device", "sequenceNumber": 0,
    "message": ["plain": ["_0": ["response": ["_1": ["handshake": ["_0": [
        "wireProtocolVersion": 19, "private_key": "synthetic-secret", "deviceIdentifier": "synthetic-id"
    ]]]]]]]
]
let response = try frame(fixture)
for split in 0..<response.count {
    let partial = try RemotePairingHandshake.response(Data(response.prefix(split)))
    precondition(partial == nil)
}
let summary = try RemotePairingHandshake.response(response)!
precondition(summary.contains("protocol 19"))
precondition(!summary.contains("synthetic-secret") && !summary.contains("synthetic-id"))
for invalid in [Data("NotPairing".utf8) + Data([0, 0]),
                Data("RPPairing".utf8) + Data([0, 0]), try frame(["originatedBy": "host"])] {
    do { _ = try RemotePairingHandshake.response(invalid); fatalError("Accepted malformed response") }
    catch {}
}
let request = try RemotePairingHandshake.request()
let json = try JSONSerialization.jsonObject(with: request.dropFirst(11)) as! [String: Any]
precondition(json["originatedBy"] as? String == "host")
precondition(!String(data: request.dropFirst(11), encoding: .utf8)!.contains("pairSetup"))
print("PASS: fragmented frames, invalid peers, output redaction, handshake-only request")

// A local IPv4 datagram. Swapping endpoints must preserve header word sum,
// reject unrelated destinations, and never forward malformed/truncated data.
var packet = [UInt8](repeating: 0, count: 40)
packet[0] = 0x45; packet[2] = 0; packet[3] = 40; packet[9] = 6
packet.replaceSubrange(12..<16, with: LocalPacketReflection.client)
packet.replaceSubrange(16..<20, with: LocalPacketReflection.device)
func wordSum(_ bytes: [UInt8]) -> Int {
    stride(from: 0, to: bytes.count, by: 2).reduce(0) { $0 + Int(bytes[$1]) * 256 + Int(bytes[$1 + 1]) }
}
let reflected = [UInt8](LocalPacketReflection.reflect(Data(packet))!)
precondition(Array(reflected[12..<16]) == LocalPacketReflection.device)
precondition(Array(reflected[16..<20]) == LocalPacketReflection.client)
precondition(wordSum(Array(packet[0..<20])) == wordSum(Array(reflected[0..<20])))
precondition(LocalPacketReflection.reflect(Data(reflected)) == Data(packet))
precondition(LocalPacketReflection.reflect(Data(packet.prefix(19))) == nil)
precondition(LocalPacketReflection.reflect(Data(packet.prefix(39))) == nil)
var unrelated = packet; unrelated[16] = 8
precondition(LocalPacketReflection.reflect(Data(unrelated)) == nil)
var invalidHeader = packet; invalidHeader[0] = 0x4f
precondition(LocalPacketReflection.reflect(Data(invalidHeader)) == nil)
print("PASS: local packet reflection, checksum preservation, malformed and unrelated packet rejection")

let signed = RunnerSigning.authorization(token: String(repeating: "a", count: 64), method: "post", target: "/tap", timestamp: 1700000000, nonce: String(repeating: "b", count: 32), body: Data(#"{"x":1,"y":2}"#.utf8))
precondition(signed == "IPU-HMAC-SHA256 ts=1700000000, nonce=\(String(repeating: "b", count: 32)), sig=dfbcd90cc34ecb273bfe9b267a62be05879c4144c58801d56e283d321c1b8dd8")
print("PASS: runner HMAC matches independent Python known answer")
