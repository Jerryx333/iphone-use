import Foundation
import Security

/// Shared only by this app and its own extension, after the first device unlock.
enum ProtectedStore {
    private static var group: String { Bundle.main.object(forInfoDictionaryKey: "IPUKeychainGroup") as? String ?? "" }
    private static func query(_ key: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: "com.leeguoo.iphone-use.selfhost",
         kSecAttrAccount as String: key, kSecAttrAccessGroup as String: group]
    }
    static func read(_ key: String) -> Data? {
        var query = query(key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }
    static func write(_ key: String, _ data: Data) throws {
        let attributes: [String: Any] = [kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly]
        let status = SecItemUpdate(query(key) as CFDictionary, attributes as CFDictionary)
        if status == errSecSuccess { return }
        guard status == errSecItemNotFound else { throw NSError(domain: "ProtectedStore", code: Int(status)) }
        var query = query(key)
        attributes.forEach { query[$0.key] = $0.value }
        let added = SecItemAdd(query as CFDictionary, nil)
        guard added == errSecSuccess else { throw NSError(domain: "ProtectedStore", code: Int(added)) }
    }
    static func token(_ key: String) throws -> String {
        if let data = read(key), let value = String(data: data, encoding: .utf8) { return value }
        let value = try freshToken()
        try write(key, Data(value.utf8))
        return value
    }
    static func freshToken() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw NSError(domain: "ProtectedStore", code: -1)
        }
        let value = bytes.map { String(format: "%02x", $0) }.joined()
        return value
    }
}
