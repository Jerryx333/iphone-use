import Foundation
import CryptoKit

enum PairRecordImport {
    static func read(_ url: URL) throws -> Data {
        let file = try FileHandle(forReadingFrom: url)
        defer { try? file.close() }
        let data = try file.read(upToCount: 65_537) ?? Data()
        try validate(data)
        return data
    }
    static func validate(_ data: Data) throws {
        guard data.count <= 65_536,
              let record = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
              let privateKey = record["private_key"] as? Data, privateKey.count == 32,
              let publicKey = record["public_key"] as? Data, publicKey.count == 32,
              let identifier = record["identifier"] as? String, UUID(uuidString: identifier) != nil,
              try Curve25519.Signing.PrivateKey(rawRepresentation: privateKey).publicKey.rawRepresentation == publicKey,
              (record["alt_irk"] == nil || (record["alt_irk"] as? Data)?.count == 16)
        else { throw NSError(domain: "PairRecordImport", code: 1) }
    }
}
