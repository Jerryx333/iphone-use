import SwiftUI
import UniformTypeIdentifiers

@MainActor
struct LocalControlSetupView: View {
    @ObservedObject var app: AppModel
    @StateObject private var tunnel = LocalTunnel()
    @State private var importing = false
    @State private var busy = false
    @State private var message: String?
    @State private var connectionCode = false

    private var completePackage: Bool {
        Bundle.main.object(forInfoDictionaryKey: "IPUSingleAppControl") as? Bool == true
    }

    private var starting: Bool {
        tunnel.state == "正在启用" ||
        ["starting", "connecting", "pairing", "resolving_runner", "starting_runner", "awaiting_status"].contains(tunnel.nativePhase)
    }

    private var hasPairing: Bool { ProtectedStore.read("pair_record") != nil }

    private var capabilityDescription: String {
        if !completePackage {
            return "此版本可以连接已配对的设备。要在这台 iPhone 上独立运行控制，请更新为包含本机控制的完整版本。"
        }
        if !DeviceStore.supportsStandaloneControl {
            return "当前系统可以使用内置浏览器和连接其他设备。本机独立控制需要 iOS 17.4 或更新版本。"
        }
        return "启用后，助手可以操作这台 iPhone，也可以让其他设备连接。运行期间无需 Mac 托管。"
    }

    var body: some View {
        Form {
            Section {
                Label(tunnel.nativePhase == "awaiting_status" ? "正在核对本机控制状态" : tunnel.nativePhase == "running" ? "本机控制已启动" : starting ? "本机控制正在启动" : "本机控制未启动",
                      systemImage: "iphone")
                Text(capabilityDescription)
                    .foregroundStyle(.secondary)
                Button("启用本机控制") {
                    Task {
                        busy = true
                        await tunnel.startAndCommand("launch")
                        busy = false
                    }
                }.disabled(busy || starting || tunnel.nativePhase == "running" || !completePackage || !DeviceStore.supportsStandaloneControl || !hasPairing)
                Button("重新启动本机控制") {
                    Task {
                        guard !busy else { return }
                        busy = true
                        message = nil
                        defer { busy = false }
                        guard let api = app.thisPhoneSession?.client else {
                            message = "尚未连接本机服务，无法确认当前任务；请先检查连接。"
                            return
                        }
                        do {
                            let tasks = try await api.recentTasks()
                            guard tasks.allSatisfy({ $0.state.isTerminal }) else {
                                message = "请先停止当前手机任务，再重新启动本机控制。"
                                return
                            }
                        } catch {
                            message = "无法核对当前手机任务，暂未重启；请检查连接后再试。"
                            return
                        }
                        app.voiceCall.end()
                        await tunnel.startAndCommand("restart")
                    }
                }.disabled(busy || starting || tunnel.state != "已启用" || !completePackage || !DeviceStore.supportsStandaloneControl || !hasPairing)
                Button("停止本机控制", role: .destructive) {
                    Task {
                        busy = true
                        await tunnel.stop()
                        busy = false
                    }
                }.disabled(busy || tunnel.state == "未启用")
                Button("让其他设备连接") { connectionCode = true }
                    .disabled(tunnel.nativePhase != "running")
                if busy { ProgressView("正在设置") }
                if let message { Text(message).foregroundStyle(.secondary) }
                if !completePackage { Text("此版本尚未包含完整的本机控制组件。") }
                else if !DeviceStore.supportsStandaloneControl {
                    Text("本机独立控制需要 iOS 17.4 或更新版本。当前系统仍可连接电脑托管的控制服务。")
                }
                else if !hasPairing {
                    Text("请先在下方导入配对记录，再启用本机控制。")
                        .foregroundStyle(.secondary)
                }
                if !tunnel.error.isEmpty || !tunnel.nativeError.isEmpty {
                    Text(tunnel.nativePhase == "awaiting_status"
                         ? "正在等待本机控制确认状态。请保持手机解锁并连接 Wi-Fi；若仍无响应，先停止控制后再启动。"
                         : "本机控制尚未就绪。请保持手机解锁并连接 Wi-Fi，再尝试启用；首次使用请先完成配对和系统网络授权。")
                        .foregroundStyle(.secondary)
                }
            }
            if completePackage && DeviceStore.supportsStandaloneControl {
                Section("首次设置") {
                    Text("首次安装与配对需要电脑。完成后只使用这个 App；系统网络授权和重启后的首次解锁由你确认。")
                    Button("导入配对记录") { message = nil; importing = true }
                        .disabled(busy || starting || tunnel.state != "未启用")
                    if tunnel.state != "未启用" {
                        Text("更换配对记录前，请先停止本机控制。")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            #if DEBUG
            Section("开发验收") {
                if #available(iOS 16.0, *) {
                    NavigationLink("验证本机控制") { ControlSmokeView() }
                }
            }
            #endif
        }
        .navigationTitle("本机控制")
        .sheet(isPresented: $connectionCode) { LocalControlConnectionCodeView() }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.data]) { result in
            do {
                let url = try result.get()
                guard completePackage, DeviceStore.supportsStandaloneControl,
                      !busy, !starting, tunnel.state == "未启用" else {
                    message = "更换配对记录前，请先停止本机控制。"; return
                }
                guard url.startAccessingSecurityScopedResource() else {
                    message = "无法读取此文件，请重新选择配对记录。"; return
                }
                defer { url.stopAccessingSecurityScopedResource() }
                var data = try PairRecordImport.read(url)
                defer { data.resetBytes(in: 0..<data.count) }
                try ProtectedStore.write("pair_record", data)
                message = "配对记录已保存，可以启用本机控制。"
            } catch {
                if (error as? CocoaError)?.code != .userCancelled {
                    message = "无法导入配对记录，请检查文件。"
                }
            }
        }
        .task {
            await tunnel.load()
            importStagedPairing()
            while !Task.isCancelled {
                await tunnel.refresh()
                if tunnel.nativePhase == "running" { _ = await app.connectStandaloneControlIfReady() }
                try? await Task.sleep(nanoseconds: 3_000_000_000)
            }
        }
    }

    /// `iphone-use setup-app` copies the first pairing record it made over USB
    /// into this app's Documents (only a trusted, developer-mode Mac can write
    /// there). Take it into the Keychain once, then delete the file. While local
    /// control runs it waits, as a manual import would.
    private func importStagedPairing() {
        let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("pair-record-import.plist")
        guard FileManager.default.fileExists(atPath: url.path),
              completePackage, DeviceStore.supportsStandaloneControl else { return }
        guard tunnel.state == "未启用" else {
            message = "电脑传来了新的配对记录。停止本机控制后重新打开此页即可导入。"
            return
        }
        defer { try? FileManager.default.removeItem(at: url) }
        do {
            var data = try PairRecordImport.read(url)
            defer { data.resetBytes(in: 0..<data.count) }
            try ProtectedStore.write("pair_record", data)
            message = "已导入电脑生成的配对记录，可以启用本机控制。"
        } catch {
            message = "电脑传来的配对记录无效，请重新运行 iphone-use setup-app。"
        }
    }
}
