import Combine
import SwiftUI

enum AppTab: String, Hashable { case devices, browser, tasks, settings }

struct MainTabsView: View {
    @ObservedObject var app: AppModel
    @State private var selection: AppTab = .devices
    @StateObject private var workspace = BrowserWorkspace()

    var body: some View {
        TabView(selection: $selection) {
            devices
                .tabItem { Label("设备", systemImage: "iphone") }
                .tag(AppTab.devices)
            BrowserWorkspaceView(settings: app.agent, workspace: workspace)
                .tabItem { Label("浏览器", systemImage: "globe") }
                .tag(AppTab.browser)
            TaskHubView(app: app, browserAgent: workspace.agent, openBrowser: { selection = .browser })
                .tabItem { Label("任务", systemImage: "checklist") }
                .tag(AppTab.tasks)
            AppSettingsView(app: app)
                .tabItem { Label("设置", systemImage: "gearshape") }
                .tag(AppTab.settings)
        }
        .tint(Theme.accent)
        .onDisappear { workspace.agent.pause() }
        .appOnChange(of: app.focusedID) { _, id in if id != nil { selection = .devices } }
        .onAppear {
            #if DEBUG
            if let name = UserDefaults.standard.string(forKey: "tab"), let tab = AppTab(rawValue: name) {
                selection = tab
            }
            #endif
        }
    }

    @ViewBuilder private var devices: some View {
        if app.devices.isEmpty {
            WelcomeView(app: app)
        } else if let session = app.focused {
            Group {
                if let group = app.sync, group.lead == session.id {
                    SyncView(app: app, lead: session, group: group)
                } else {
                    RemoteView(app: app, session: session)
                }
            }.id(session.id)
        } else {
            DeviceGridView(app: app)
        }
    }
}

struct TaskHubView: View {
    @ObservedObject var app: AppModel
    @ObservedObject var browserAgent: BrowserAgent
    var openBrowser: () -> Void
    @State private var flows = false

    var body: some View {
        AppNavigationStack {
            List {
                Section {
                    Button { app.openAgent() } label: {
                        Label("新建任务", systemImage: "square.and.pencil")
                    }.accessibilityIdentifier("new-task")
                    Button { flows = true } label: {
                        Label("流程与定时", systemImage: "clock")
                    }.disabled(app.agent.target == nil)
                } footer: {
                    Text("选择已配对的 iPhone，让 agent 执行任务，或运行保存的流程。")
                }
                Section("浏览器") {
                    Button(action: openBrowser) {
                        Label(browserAgent.goal.isEmpty ? String(localized: "新建浏览器任务") : browserAgent.goal, systemImage: "globe")
                            .lineLimit(2)
                    }
                    if browserAgent.state == .running { ProgressView("正在操作网页") }
                    if browserAgent.state == .confirming { Text("等待确认").foregroundStyle(.secondary) }
                    if let message = browserAgent.message { Text(message).font(.footnote).foregroundStyle(.secondary) }
                }
                Section("手机任务") {
                    if app.agent.history.isEmpty {
                        Text("还没有任务").foregroundStyle(.secondary)
                    } else {
                        ForEach(app.agent.history) { task in
                            Button { app.openAgent() } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(task.goal).foregroundStyle(.primary)
                                    Text(task.state.title).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("任务")
            .task { app.agent.chooseDefaultTarget(); await app.agent.refreshHistory() }
            .refreshable { await app.agent.refreshHistory() }
            .sheet(isPresented: $flows) {
                if let session = app.agent.target { FlowsHubView(session: session) }
            }
        }
    }
}

struct AppSettingsView: View {
    @ObservedObject var app: AppModel
    @State private var devices = false

    var body: some View {
        AppNavigationStack {
            Form {
                Section("任务") {
                    NavigationLink { ModelSettingsView(agent: app.agent) } label: {
                        Label("模型与服务", systemImage: "sparkles")
                    }
                }
                Section("设备") {
                    Button { devices = true } label: {
                        Label("管理设备", systemImage: "iphone")
                    }
                    Picker("画面", selection: $app.videoPreference) {
                        ForEach(VideoPreference.allCases, id: \.self) { Text($0.title).tag($0) }
                    }
                }
                Section("关于") {
                    AppLabeledContent("版本", value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "")
                    Link(destination: URL(string: "https://github.com/leeguooooo/iphone-use")!) {
                        Label("项目主页", systemImage: "arrow.up.right.square")
                    }
                }
            }
            .navigationTitle("设置")
            .sheet(isPresented: $devices) { DevicesSheet(app: app) }
        }
    }
}
