import CryptoKit
import Foundation
@preconcurrency import LocalAuthentication
import Security
import SwiftUI
import UIKit
import UserNotifications

struct OwnerSheet: Decodable, Identifiable {
    let id: String
    let kind: String
    let summary: String
    let details: [String]
    let nonce: String
    let digest: String
    let expires_at_ms: UInt64
    var message: Data { Data("iphone-use-owner-confirm/v1\n\(id)\n\(nonce)\n\(digest)\n\(kind)".utf8) }
}

// Main-thread presentation state is read synchronously by the remote action worker.
// It is deliberately separate from XCTest readiness and the local self-test controls.
enum OwnerUIProtection {
    static var presented = false
    static func blocks(action: String, args: [String: Any], foregroundPID: Int32?) -> Bool {
        guard ["tap", "type", "swipe", "press", "launch", "open_url", "unlock"].contains(action) else { return false }
        var blocked = false
        let check = {
            blocked = presented && (UIApplication.shared.applicationState != .background
                || args["bundle_id"] as? String == Bundle.main.bundleIdentifier
                || foregroundPID == getpid())
        }
        if Thread.isMainThread { check() } else { DispatchQueue.main.sync(execute: check) }
        return blocked
    }
}

@objc(SelfRunnerOwnerProtection)
final class SelfRunnerOwnerProtection: NSObject {
    @objc(blocksInput:) static func blocksInput(_ args: NSDictionary) -> NSNumber {
        NSNumber(value: OwnerUIProtection.blocks(action: "tap", args: args as? [String: Any] ?? [:], foregroundPID: (args["pid"] as? NSNumber)?.int32Value))
    }
}

enum OwnerKey {
    private static let tag = Data("dev.iphoneuse.selfrunner.owner/v1".utf8)
    enum Failure: Error, LocalizedError {
        case faceIDRequired, authenticationFailed, keyUnavailable, signingFailed
        var errorDescription: String? {
            switch self {
            case .faceIDRequired: return "必须使用已登记的面容 ID；不支持设备密码回退。"
            case .authenticationFailed: return "面容 ID 未通过或已取消。"
            case .keyUnavailable: return "主人密钥不可用。面容登记变化后，须由主人在 Mac 本地恢复绑定。"
            case .signingFailed: return "主人签名失败。"
            }
        }
    }
    private static func key(create: Bool, context: LAContext) throws -> SecKey {
        var result: CFTypeRef?
        let query: [CFString: Any] = [kSecClass: kSecClassKey, kSecAttrApplicationTag: tag,
            kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom, kSecReturnRef: true,
            kSecUseAuthenticationContext: context]
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecSuccess, let result { return (result as! SecKey) }
        guard create, status == errSecItemNotFound else { throw Failure.keyUnavailable }
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            [.privateKeyUsage, .biometryCurrentSet], &error) else { throw Failure.keyUnavailable }
        let attributes: [CFString: Any] = [kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits: 256, kSecAttrTokenID: kSecAttrTokenIDSecureEnclave,
            kSecUseAuthenticationContext: context,
            kSecPrivateKeyAttrs: [kSecAttrIsPermanent: true, kSecAttrApplicationTag: tag, kSecAttrAccessControl: access]]
        guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error) else { throw Failure.keyUnavailable }
        return key
    }
    private static func publicData(_ key: SecKey) throws -> Data {
        var error: Unmanaged<CFError>?
        guard let publicKey = SecKeyCopyPublicKey(key),
              let bytes = SecKeyCopyExternalRepresentation(publicKey, &error) else { throw Failure.keyUnavailable }
        return bytes as Data
    }
    static func fingerprint(_ bytes: Data) -> String { SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined() }
    static func localFingerprint() -> String? {
        let context = LAContext()
        context.interactionNotAllowed = true
        defer { context.invalidate() }
        return try? fingerprint(publicData(key(create: false, context: context)))
    }
    // A fresh, biometric-only context for every explicit button press; no reusable auth window.
    // The worker keeps potentially blocking Security calls off the transport/main queue.
    static func signature(sheet: OwnerSheet?) async throws -> (publicKey: String, signature: String) {
        let context = LAContext()
        context.localizedFallbackTitle = ""
        context.touchIDAuthenticationAllowableReuseDuration = 0
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &error),
              context.biometryType == .faceID else { throw Failure.faceIDRequired }
        defer { context.invalidate() }
        do {
            guard try await context.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics,
                localizedReason: sheet == nil ? "绑定 Self Runner 主人设备" : "确认手机上显示的完整操作详情") else {
                throw Failure.authenticationFailed
            }
        } catch { throw Failure.authenticationFailed }
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    let key = try key(create: sheet == nil, context: context)
                    let publicKey = try publicData(key).base64EncodedString()
                    let message = sheet?.message ?? Data("iphone-use-owner-key/v1\n\(publicKey)".utf8)
                    var error: Unmanaged<CFError>?
                    guard let signature = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256,
                        message as CFData, &error) else { throw Failure.signingFailed }
                    continuation.resume(returning: (publicKey, (signature as Data).base64EncodedString()))
                } catch { continuation.resume(throwing: error) }
            }
        }
    }
}

final class OwnerConfirmationModel: NSObject, ObservableObject, UNUserNotificationCenterDelegate {
    @Published var presented = false { didSet { OwnerUIProtection.presented = presented || busy } }
    @Published private(set) var pending: [OwnerSheet] = []
    @Published private(set) var bound: Bool?
    @Published private(set) var daemonFingerprint = ""
    @Published private(set) var localFingerprint = ""
    @Published private(set) var message = ""
    @Published private(set) var busy = false { didSet { OwnerUIProtection.presented = presented || busy } }
    private var link: RemoteLink?
    private var seen: Set<String> = []
    private var epoch = 0
    private var notificationIDs: Set<String> = []
    private var selectedID: String?
    var orderedPending: [OwnerSheet] {
        guard let selectedID else { return pending }
        return pending.filter { $0.id == selectedID } + pending.filter { $0.id != selectedID }
    }
    func attach(_ link: RemoteLink) {
        self.link = link
        UNUserNotificationCenter.current().delegate = self
        link.ownerPending = { [weak self] in self?.receive($0) }
        link.ownerDisconnected = { [weak self] in
            guard let self else { return }
            self.epoch += 1; self.pending = []; self.seen = []
            self.bound = nil; self.daemonFingerprint = ""
            self.removeNotifications()
        }
    }
    private func removeNotifications() {
        let center = UNUserNotificationCenter.current()
        center.removePendingNotificationRequests(withIdentifiers: Array(notificationIDs))
        center.removeDeliveredNotifications(withIdentifiers: Array(notificationIDs))
        notificationIDs = []
    }
    func open() {
        presented = true
        enableNotifications()
        perform { try await self.refresh() }
    }
    func enableNotifications() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { allowed, _ in
            if !allowed { DispatchQueue.main.async { self.message = "本地通知未获授权；在线前台仍会弹出确认页。" } }
        }
    }
    private func receive(_ data: Data) {
        struct Pending: Decodable { let pending: [OwnerSheet] }
        guard let result = try? JSONDecoder().decode(Pending.self, from: data) else {
            message = "owner_pending_invalid"; return
        }
        pending = result.pending
        let current = Set(pending.map(\.id))
        let obsolete = notificationIDs.subtracting(Set(current.map { "owner-" + $0 }))
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: Array(obsolete))
        UNUserNotificationCenter.current().removePendingNotificationRequests(withIdentifiers: Array(obsolete))
        notificationIDs.subtract(obsolete)
        let new = pending.filter { !seen.contains($0.id) }
        seen = current
        guard !new.isEmpty else { return }
        if UIApplication.shared.applicationState == .active {
            selectedID = new.first?.id
            presented = true
            if !busy { perform { try await self.refreshKey() } }
        } else {
            for sheet in new {
                let content = UNMutableNotificationContent()
                content.title = "Self Runner 主人确认"
                // Do not put nonce, signature, or private details on the lock screen.
                content.body = "有新的操作等待面容 ID 确认，请打开查看完整详情。"
                content.userInfo = ["owner_id": sheet.id]
                let id = "owner-" + sheet.id
                notificationIDs.insert(id)
                UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
            }
        }
    }
    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler completionHandler: @escaping () -> Void) {
        let id = response.notification.request.content.userInfo["owner_id"] as? String
        DispatchQueue.main.async {
            self.selectedID = id
            self.open()
            completionHandler()
        }
    }
    @MainActor private func request(_ method: String, _ path: String, body: [String: Any]? = nil) async throws -> Data {
        guard let link else { throw RemoteLink.OwnerLinkError.code("owner_offline") }
        let data = try body.map { try JSONSerialization.data(withJSONObject: $0) } ?? Data()
        let (status, response) = try await link.ownerRequest(method: method, path: path, body: data)
        guard (200...299).contains(status) else {
            // Daemon errors are shown verbatim, only in this view, never sent to any logger.
            throw RemoteLink.OwnerLinkError.code("HTTP \(status)\n" + (String(data: response, encoding: .utf8) ?? "owner_invalid_response"))
        }
        return response
    }
    @MainActor private func refreshKey() async throws {
        struct KeyStatus: Decodable { let bound: Bool; let fingerprint: String? }
        let data = try await request("GET", "/owner/key")
        let status = try JSONDecoder().decode(KeyStatus.self, from: data)
        bound = status.bound; daemonFingerprint = status.fingerprint ?? ""
        localFingerprint = OwnerKey.localFingerprint() ?? ""
    }
    @MainActor private func refresh() async throws {
        try await refreshKey()
        receive(try await request("GET", "/owner/pending"))
    }
    func reload() { perform { try await self.refresh() } }
    func bind() {
        perform {
            try await self.refreshKey()
            if self.bound == true && self.localFingerprint != self.daemonFingerprint {
                throw RemoteLink.OwnerLinkError.code("已绑定另一把主人钥匙；换钥必须由旧钥签名。请由主人处理，不能覆盖绑定。")
            }
            let epoch = self.epoch
            let signed = try await OwnerKey.signature(sheet: nil)
            guard epoch == self.epoch else { throw RemoteLink.OwnerLinkError.code("owner_connection_changed") }
            _ = try await self.request("POST", "/owner/key", body: ["public_key": signed.publicKey, "signature": signed.signature])
            try await self.refreshKey()
            self.message = "绑定成功，请核对两端公钥指纹。"
        }
    }
    func answer(_ sheet: OwnerSheet, approve: Bool) {
        perform {
            let epoch = self.epoch
            var body: [String: Any] = ["approve": approve]
            if approve {
                let signed = try await OwnerKey.signature(sheet: sheet)
                guard epoch == self.epoch else { throw RemoteLink.OwnerLinkError.code("owner_connection_changed") }
                body["signature"] = signed.signature
            }
            // Never replay or automatically retry this request, including an unknown outcome.
            _ = try await self.request("POST", "/owner/confirm/\(sheet.id)", body: body)
            self.message = approve ? "已提交主人签名。" : "已拒绝。"
            self.receive(try await self.request("GET", "/owner/pending"))
        }
    }
    private func perform(_ operation: @escaping @MainActor () async throws -> Void) {
        guard !busy else { return }
        busy = true; message = ""
        Task { @MainActor in
            defer { busy = false }
            do { try await operation() }
            catch let error as RemoteLink.OwnerLinkError { message = error.localizedDescription }
            catch let error as OwnerKey.Failure { message = error.localizedDescription }
            catch { message = "owner_response_invalid" }
        }
    }
}

struct OwnerConfirmationView: View {
    @ObservedObject var model: OwnerConfirmationModel
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(model.bound == true ? "守护进程已绑定主人公钥" : (model.bound == false ? "尚未绑定主人设备" : "绑定状态未读取"))
                    if !model.daemonFingerprint.isEmpty { Text("守护进程指纹（SHA-256）：\n" + model.daemonFingerprint) }
                    if !model.localFingerprint.isEmpty { Text("本机公钥指纹（SHA-256）：\n" + model.localFingerprint) }
                    Button("绑定为主人设备（面容 ID）") { model.bind() }
                    Button("启用待确认本地通知") { model.enableNotifications() }
                    Button("刷新") { model.reload() }
                    if !model.message.isEmpty { Text(model.message).foregroundStyle(.red) }
                    if model.pending.isEmpty { Text("当前没有待确认单。") }
                    ForEach(model.orderedPending) { sheet in
                        Divider()
                        Text(sheet.summary).font(.headline)
                        Text("类型：\(sheet.kind)\n编号：\(sheet.id)")
                        Text("有效期至：" + Date(timeIntervalSince1970: Double(sheet.expires_at_ms) / 1000).formatted())
                        ForEach(Array(sheet.details.enumerated()), id: \.offset) { _, detail in Text(detail) }
                        Button("允许（面容 ID）") { model.answer(sheet, approve: true) }
                        Button("拒绝", role: .destructive) { model.answer(sheet, approve: false) }
                    }
                }
                .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading).padding()
            }
            .disabled(model.busy)
            .navigationTitle("主人确认")
            .toolbar { Button("关闭") { model.presented = false }.disabled(model.busy) }
        }
        .interactiveDismissDisabled(model.busy)
    }
}
