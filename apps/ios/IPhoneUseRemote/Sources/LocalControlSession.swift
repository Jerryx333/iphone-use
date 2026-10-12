import Foundation
@preconcurrency import NetworkExtension

@MainActor final class LocalTunnel: ObservableObject {
    @Published var state = "未启用"
    @Published var error = ""
    @Published var rows: [ResultRow] = []
    @Published var nativePhase = "未启动"
    @Published var nativeError = ""
    private var manager: NETunnelProviderManager?
    static var bundle: String {
        Bundle.main.object(forInfoDictionaryKey: "IPUTunnelBundle") as? String
            ?? "com.leeguoo.iphone-use.selfhost-probe.tunnel"
    }
    func load() async {
        do {
            let managers = try await NETunnelProviderManager.loadAllFromPreferences()
            manager = managers.first { ($0.protocolConfiguration as? NETunnelProviderProtocol)?.providerBundleIdentifier == Self.bundle }
            await refresh()
        } catch { self.error = error.localizedDescription }
    }
    func start() async {
        do {
            if manager == nil { await load() }
            let manager = manager ?? NETunnelProviderManager()
            let config = NETunnelProviderProtocol()
            config.providerBundleIdentifier = Self.bundle
            config.serverAddress = "10.7.0.1"
            manager.protocolConfiguration = config
            manager.localizedDescription = "Phone Use 本机开发网络"
            manager.isEnabled = true
            manager.isOnDemandEnabled = true
            let rule = NEOnDemandRuleConnect()
            rule.interfaceTypeMatch = .any
            manager.onDemandRules = [rule]
            try await manager.saveToPreferences()
            try await manager.loadFromPreferences()
            self.manager = manager
            try manager.connection.startVPNTunnel()
            state = "正在启用"
        } catch { self.error = error.localizedDescription }
    }
    func stop() async {
        guard let manager else { return }
        // Ending XCTest also ends its host app. Persist the user's intent and
        // disable automatic reconnect before the extension shuts the host down.
        do { try ProtectedStore.write("session_enabled", Data([0])) }
        catch { self.error = error.localizedDescription; return }
        manager.isOnDemandEnabled = false
        do { try await manager.saveToPreferences() }
        catch { self.error = error.localizedDescription; return }
        manager.connection.stopVPNTunnel()
    }
    func startAndCommand(_ command: String) async {
        if manager?.connection.status != .connected { await start() }
        for _ in 0..<60 {
            if Task.isCancelled { return }
            if manager?.connection.status == .connected { await message(command); return }
            try? await Task.sleep(nanoseconds: 500_000_000)
        }
        nativeError = "本机开发网络尚未就绪"
    }
    func command(_ command: String) async {
        await message(command)
    }
    private func message(_ command: String) async {
        guard let manager, manager.connection.status == .connected, let session = manager.connection as? NETunnelProviderSession else { return }
        await receive(session, command: command)
    }
    private func receive(_ session: NETunnelProviderSession, command: String) async {
        let response: Data? = await withCheckedContinuation { continuation in
            do { try session.sendProviderMessage(Data(command.utf8)) { continuation.resume(returning: $0) } }
            catch { continuation.resume(returning: nil) }
        }
        if let response, let result = (try? JSONSerialization.jsonObject(with: response)) as? [String: Any] {
            var report = result
            report["observed_at"] = ISO8601DateFormatter().string(from: Date())
            report["executor"] = "on-device app reading its own Network Extension"
            if let data = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]) {
                let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
                try? data.write(to: documents.appendingPathComponent("self-host-runtime.json"), options: [.atomic, .completeFileProtection])
            }
            if let data = try? JSONSerialization.data(withJSONObject: result["probes"] ?? []),
               let decoded = try? JSONDecoder().decode([ResultRow].self, from: data) { rows = decoded }
            if let native = result["native"] as? [String: Any] {
                nativePhase = native["phase"] as? String ?? "unknown"
                nativeError = native["error_code"] as? String ?? ""
            }
            if let error = result["error_code"] as? String { nativeError = error }
        }
    }
    func refresh() async {
        guard let manager else { return }
        switch manager.connection.status {
        case .connected: state = "已启用"
        case .connecting: state = "正在启用"
        case .disconnecting: state = "正在停止"
        default: state = "未启用"
        }
        guard manager.connection.status == .connected, let session = manager.connection as? NETunnelProviderSession else { return }
        await receive(session, command: "status")
    }
}
