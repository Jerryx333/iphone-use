import Foundation
import Security
import CryptoKit

struct RemotePairing: Codable {
    let v: Int
    let host: String
    let port: UInt16
    let cert_sha256: String
    let psk: String
    var ws_url: String? = nil
    let device_id: String
    var route: String {
        HMAC<SHA256>.authenticationCode(for: Data("selfrunner-ws-route-v1".utf8), using: SymmetricKey(data: key))
            .prefix(16).map { String(format: "%02x", $0) }.joined()
    }
    var webSocketURL: URL? {
        guard let ws_url else { return nil }
        return URL(string: ws_url.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + "/selfrunner/v1/" + route)
    }
    static func validWebSocketBase(_ value: String) -> Bool {
        guard value.utf8.count <= 512,
              let parts = URLComponents(string: value),
              parts.scheme == "wss" || parts.scheme == "ws",
              let host = parts.host, !host.isEmpty, host.utf8.count <= 253,
              host.unicodeScalars.allSatisfy({ CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-:_").contains($0) }),
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty || parts.path == "/",
              parts.port == nil || (1...65535).contains(parts.port!),
              !value.contains("%"), !value.contains("\\"),
              value.rangeOfCharacter(from: .whitespacesAndNewlines) == nil else { return false }
        // Reject an empty port, which URLComponents otherwise normalizes away.
        let authority = value.dropFirst(parts.scheme!.count + 3).split(separator: "/", omittingEmptySubsequences: false)[0]
        guard !authority.hasSuffix(":") else { return false }
        return parts.scheme == "wss" || host == "127.0.0.1" || host == "localhost"
    }
    var key: Data { Data(base64Encoded: psk)! }
    var pin: Data {
        Data(stride(from: 0, to: 64, by: 2).map { offset in
            let start = cert_sha256.index(cert_sha256.startIndex, offsetBy: offset)
            return UInt8(cert_sha256[start..<cert_sha256.index(start, offsetBy: 2)], radix: 16)!
        })
    }
    /// Request-signing token for the in-app runner API (RunnerCore.bundle), derived from the PSK so
    /// the Mac side can compute it without it ever crossing the network (scripts/selfrunner-runner-token.py).
    var runnerToken: String {
        let mac = HMAC<SHA256>.authenticationCode(for: Data("selfrunner-runner-token-v1".utf8), using: SymmetricKey(data: key))
        return mac.map { String(format: "%02x", $0) }.joined()
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
              Data(base64Encoded: pair.psk)?.count == 32,
              pair.ws_url.map(Self.validWebSocketBase) ?? true else { throw Failure.code("invalid_pairing") }
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
