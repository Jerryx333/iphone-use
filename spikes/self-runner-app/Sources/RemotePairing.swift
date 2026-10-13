import Foundation
import Security

struct RemotePairing: Codable {
    let v: Int
    let host: String
    let port: UInt16
    let cert_sha256: String
    let psk: String
    let device_id: String
    var key: Data { Data(base64Encoded: psk)! }
    var pin: Data {
        Data(stride(from: 0, to: 64, by: 2).map { offset in
            let start = cert_sha256.index(cert_sha256.startIndex, offsetBy: offset)
            return UInt8(cert_sha256[start..<cert_sha256.index(start, offsetBy: 2)], radix: 16)!
        })
    }
    private static let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: "dev.iphoneuse.selfrunner.gateway", kSecAttrAccount as String: "pairing"]
    enum Failure: Error { case code(String) }
    static func validate(_ data: Data) throws -> RemotePairing {
        guard data.count <= 4096, let pair = try? JSONDecoder().decode(Self.self, from: data),
              pair.v == 1, pair.port > 0,
              !pair.host.isEmpty, pair.host.utf8.count <= 253,
              !pair.host.contains(where: { $0.isWhitespace || $0.isNewline }),
              pair.host.unicodeScalars.allSatisfy({ CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-:_").contains($0) }),
              pair.cert_sha256.range(of: "^[0-9a-fA-F]{64}$", options: .regularExpression) != nil,
              pair.device_id.range(of: "^[0-9a-fA-F]{32}$", options: .regularExpression) != nil,
              Data(base64Encoded: pair.psk)?.count == 32 else { throw Failure.code("invalid_pairing") }
        return pair
    }
    static func loadOrImport(documents: URL) throws -> RemotePairing? {
        var query = Self.query
        query[kSecReturnData as String] = true
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecSuccess {
            guard let data = item as? Data else { throw Failure.code("keychain_data") }
            return try validate(data)
        }
        guard status == errSecItemNotFound else { throw Failure.code("keychain_\(status)") }
        let url = documents.appendingPathComponent("gateway-pair.json")
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        guard let file = try? FileHandle(forReadingFrom: url) else { throw Failure.code("pairing_read") }
        defer { try? file.close() }
        guard let data = try? file.read(upToCount: 4097) else { throw Failure.code("pairing_read") }
        let pair = try validate(data)
        var add = Self.query
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let written = SecItemAdd(add as CFDictionary, nil)
        guard written == errSecSuccess else { throw Failure.code("keychain_\(written)") }
        guard (try? FileManager.default.removeItem(at: url)) != nil else { throw Failure.code("pairing_delete") }
        return pair
    }
    static func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw Failure.code("keychain_\(status)") }
    }
}
