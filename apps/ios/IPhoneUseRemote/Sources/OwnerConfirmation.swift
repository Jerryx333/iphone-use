import Foundation
import LocalAuthentication
import Security
import SwiftUI
import UserNotifications

struct OwnerConfirmation: Decodable, Identifiable, Sendable {
    let id: String
    let kind: String
    let summary: String
    let digest: String
    let nonce: String
    let object: ObjectBinding
    struct ObjectBinding: Decodable, Sendable {
        let taskID: String?
        enum CodingKeys: String, CodingKey { case taskID = "task_id" }
    }
    var message: Data { Data("iphone-use-owner-confirm/v1\n\(id)\n\(nonce)\n\(digest)\n\(kind)".utf8) }
}

/// No software fallback and no device passcode fallback. A biometric enrollment
/// change invalidates the key. The daemon's TOFU binding must then be recovered
/// locally (arbitrary local shell execution is outside the threat model).
enum OwnerSigningKey {
    private static let tag = Data("iphone-use-owner/v1".utf8)
    private static func key() throws -> SecKey {
        var result: CFTypeRef?
        let query: [CFString: Any] = [kSecClass: kSecClassKey, kSecAttrApplicationTag: tag,
                                    kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom, kSecReturnRef: true]
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess, let result { return (result as! SecKey) }
        guard status == errSecItemNotFound else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
                                                          [.privateKeyUsage, .biometryCurrentSet], &error) else {
            throw error!.takeRetainedValue() as Error
        }
        let attributes: [CFString: Any] = [kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits: 256, kSecAttrTokenID: kSecAttrTokenIDSecureEnclave,
            kSecPrivateKeyAttrs: [kSecAttrIsPermanent: true, kSecAttrApplicationTag: tag, kSecAttrAccessControl: access]]
        guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else {
            throw error!.takeRetainedValue() as Error
        }
        return key
    }
    static func publicKey() throws -> Data {
        var error: Unmanaged<CFError>?
        guard let publicKey = SecKeyCopyPublicKey(try key()),
              let data = SecKeyCopyExternalRepresentation(publicKey, &error) else {
            throw NSError(domain: "OwnerSigningKey", code: 1)
        }
        return data as Data
    }
    static func sign(_ message: Data) throws -> Data {
        var error: Unmanaged<CFError>?
        // SecKey evaluates biometryCurrentSet for this signing operation.
        guard let signature = SecKeyCreateSignature(try key(), .ecdsaSignatureMessageX962SHA256,
                                                   message as CFData, &error) else {
            throw error!.takeRetainedValue() as Error
        }
        return signature as Data
    }
}

extension DaemonClient {
    func ownerPending() async throws -> [OwnerConfirmation] {
        let (data, response) = try await send(request("owner/pending"))
        try Self.checkAgent(response, data)
        struct Pending: Decodable { let pending: [OwnerConfirmation] }
        return try JSONDecoder().decode(Pending.self, from: data).pending
    }
    func ownerPost(_ path: String, body: [String: Any]) async throws {
        var req = request(path)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try await send(req, repeatable: false)
        try Self.checkAgent(response, data)
    }
    func bindOwner() async throws {
        let publicKey = try OwnerSigningKey.publicKey().base64EncodedString()
        let signature = try OwnerSigningKey.sign(Data("iphone-use-owner-key/v1\n\(publicKey)".utf8))
        try await ownerPost("owner/key", body: ["public_key": publicKey, "signature": signature.base64EncodedString()])
    }
    func ownerAnswer(_ sheet: OwnerConfirmation, approve: Bool) async throws {
        var body: [String: Any] = ["approve": approve]
        if approve { body["signature"] = try OwnerSigningKey.sign(sheet.message).base64EncodedString() }
        try await ownerPost("owner/confirm/\(sheet.id)", body: body)
    }
}

struct OwnerConfirmationView: View {
    let client: DaemonClient
    @Environment(\.scenePhase) private var scenePhase
    @State private var pending: [OwnerConfirmation] = []
    @State private var seen: Set<String> = []
    @State private var error = ""
    @State private var busy = false
    var body: some View {
        List {
            Button("绑定为主人设备（面容 ID）") { perform { try await client.bindOwner() } }
            if !error.isEmpty { Text(error).foregroundStyle(.red) }
            ForEach(pending) { sheet in
                Section {
                    Text(sheet.summary)
                    Button("允许（面容 ID）") { perform { try await client.ownerAnswer(sheet, approve: true) } }
                    Button("拒绝", role: .destructive) { perform { try await client.ownerAnswer(sheet, approve: false) } }
                }
            }
        }
        .disabled(busy)
        .navigationTitle("主人确认")
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])
            while !Task.isCancelled {
                do {
                    pending = try await client.ownerPending()
                    for sheet in pending where seen.insert(sheet.id).inserted {
                        let content = UNMutableNotificationContent()
                        content.title = "iPhone Use 主人确认"
                        content.body = sheet.summary
                        try? await UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: sheet.id, content: content, trigger: nil))
                    }
                } catch { self.error = error.localizedDescription }
                do { try await Task.sleep(nanoseconds: 3_000_000_000) } catch { return }
            }
        }
    }
    private func perform(_ operation: @escaping @MainActor () async throws -> Void) {
        busy = true
        Task { @MainActor in
            defer { busy = false }
            do { try await operation(); pending = try await client.ownerPending(); error = "" }
            catch { self.error = error.localizedDescription }
        }
    }
}

/// Mounted at the app root so sheets are discovered throughout the foreground
/// session, rather than only while the owner-confirmation screen is open.
struct OwnerPendingWatcher: ViewModifier {
    @ObservedObject var app: AppModel
    @Environment(\.scenePhase) private var scenePhase
    @State private var seen: Set<String> = []
    func body(content: Content) -> some View {
        content.task(id: scenePhase) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                let clients = app.sessions.values.compactMap(\.client)
                for client in clients {
                    guard let sheets = try? await client.ownerPending() else { continue }
                    for sheet in sheets where seen.insert(client.publicBase.absoluteString + sheet.id).inserted {
                        let notification = UNMutableNotificationContent()
                        notification.title = "iPhone Use 主人确认"
                        notification.body = sheet.summary
                        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])
                        try? await UNUserNotificationCenter.current().add(UNNotificationRequest(
                            identifier: sheet.id, content: notification, trigger: nil))
                    }
                }
                do { try await Task.sleep(nanoseconds: 3_000_000_000) } catch { return }
            }
        }
    }
}
