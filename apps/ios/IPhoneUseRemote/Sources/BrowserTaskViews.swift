import SwiftUI
import Observation

@MainActor @Observable
final class BrowserWorkspace {
    let browser: BrowserSession
    let agent: BrowserAgent
    init() {
        let browser = BrowserSession()
        self.browser = browser
        agent = BrowserAgent(browser: browser)
    }
}

struct BrowserWorkspaceView: View {
    @Bindable var settings: AgentController
    @Bindable var workspace: BrowserWorkspace
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        BrowserView(session: workspace.browser, agent: workspace.agent, settings: settings)
            .onChange(of: scenePhase) { _, phase in
                if phase != .active { workspace.agent.pause() }
            }
    }
}

struct BrowserTaskBar: View {
    @Bindable var agent: BrowserAgent
    @Bindable var settings: AgentController
    @AppStorage("browser.goalDraft.v1") private var draft = ""
    @State private var details = false
    @FocusState private var composing: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if agent.state == .idle || agent.state == .done || agent.state == .failed || agent.state == .stopped {
                HStack {
                    TextField("让 agent 操作网页", text: $draft, axis: .vertical)
                        .lineLimit(1...3).focused($composing)
                        .accessibilityIdentifier("browser-goal")
                    Button {
                        composing = false
                        agent.start(draft, settings: settings.settings)
                    } label: { Image(systemName: "arrow.up.circle.fill").font(.title2) }
                        .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        .accessibilityLabel("开始浏览器任务")
                        .accessibilityIdentifier("browser-start-task")
                }
                if let message = agent.message {
                    Button { details = true } label: { Text(message).font(.footnote).lineLimit(2).foregroundStyle(.secondary) }
                        .accessibilityIdentifier("browser-task-result")
                } else {
                    Text("任务会把当前网页内容发送给设置中启用的模型。")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            } else {
                HStack(spacing: 12) {
                    if agent.state == .running { ProgressView() }
                    Button { details = true } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(agent.goal).font(.subheadline).lineLimit(1)
                            Text(status).font(.caption).foregroundStyle(.secondary)
                        }
                    }.buttonStyle(.plain)
                    Spacer(minLength: 0)
                    if agent.state == .paused {
                        Button("继续") { agent.resume() }.accessibilityIdentifier("browser-resume")
                    } else if agent.state == .confirming {
                        Button("查看") { details = true }
                    } else {
                        Button("接手") { agent.pause() }.accessibilityIdentifier("browser-pause")
                    }
                    Button { agent.stop() } label: { Image(systemName: "stop.circle") }
                        .accessibilityLabel("停止浏览器任务")
                        .accessibilityIdentifier("browser-stop")
                }
            }
        }
        .padding(12).background(.bar)
        .sheet(isPresented: $details) { BrowserTaskDetail(agent: agent) }
        .onAppear {
            #if DEBUG
            if UserDefaults.standard.bool(forKey: "clearBrowserDraft") { draft = "" }
            #endif
        }
        .onChange(of: agent.state) { _, state in if state == .confirming { details = true } }
    }

    private var status: String {
        switch agent.state {
        case .running: return String(localized: "正在执行，第 \(agent.steps.count + 1) 步")
        case .confirming: return String(localized: "等待确认")
        case .paused: return agent.message ?? String(localized: "已暂停")
        default: return agent.message ?? ""
        }
    }
}

struct BrowserTaskDetail: View {
    @Bindable var agent: BrowserAgent
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("任务") { Text(agent.goal) }
                Section("执行步骤") {
                    ForEach(agent.steps) { step in Text(step.text) }
                    if agent.state == .running { ProgressView("正在读取网页并决定下一步…") }
                }
                if let pending = agent.pending {
                    Section("需要确认") {
                        Text(pending.explanation)
                        Button("允许这一步") { agent.approve(); dismiss() }
                            .accessibilityIdentifier("browser-approve")
                        Button("接手操作") { agent.pause(); dismiss() }
                    }
                }
                if let message = agent.message { Section { Text(message).textSelection(.enabled) } }
                if agent.active || agent.state == .paused {
                    Section { Button("停止任务", role: .destructive) { agent.stop(); dismiss() } }
                }
            }
            .navigationTitle("浏览器任务").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("完成") { dismiss() } } }
        }
    }
}
