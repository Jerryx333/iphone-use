import Foundation
import Combine

struct BrowserDecision: Codable, Sendable {
    enum Action: String, Codable, Sendable { case open, click, fill, select, scroll, back, wait, done, handoff }
    let action: Action
    let explanation: String
    var element: Int?
    var text: String?
    var url: String?
    var requiresConfirmation: Bool?
}

struct BrowserAgentStep: Identifiable, Sendable {
    let id = UUID()
    let text: String
    var action: String? = nil
}

@MainActor
protocol BrowserReasoning {
    func decide(goal: String, page: BrowserPage?, steps: [BrowserAgentStep], models: [ModelsPayload.Assist]) async throws -> BrowserDecision
}

@MainActor
final class BrowserAgent: ObservableObject {
    enum State: Equatable { case idle, running, confirming, paused, done, failed, stopped }
    @Published private(set) var state = State.idle
    @Published private(set) var goal = ""
    @Published private(set) var steps: [BrowserAgentStep] = []
    @Published private(set) var message: String?
    @Published private(set) var pending: BrowserDecision?
    @Published private var pendingPage: BrowserPage?
    @Published private var models: [ModelsPayload.Assist] = []
    @Published private var run: Task<Void, Never>?
    @Published private var generation = UUID()
    private let browser: BrowserSession
    private let reasoning: any BrowserReasoning

    init(browser: BrowserSession, reasoning: any BrowserReasoning = BrowserModelClient()) {
        self.browser = browser
        self.reasoning = reasoning
    }

    var active: Bool { state == .running || state == .confirming }

    func start(_ input: String, settings: ModelSettings) {
        guard !active else { return }
        let text = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        let configured = settings.payload(key: ModelSettingsStore.secret)?.assist ?? []
        guard !configured.isEmpty else {
            state = .failed
            message = String(localized: "先在设置里启用浏览器要使用的模型")
            return
        }
        goal = text
        models = configured
        steps = []
        message = nil
        pending = nil
        launch()
    }

    func pause() {
        guard active else { return }
        invalidate()
        pending = nil
        pendingPage = nil
        state = .paused
        message = String(localized: "已暂停，可以手动操作网页。点继续后重新读取页面。")
    }

    func stop() {
        invalidate()
        pending = nil
        pendingPage = nil
        state = .stopped
        message = String(localized: "任务已停止")
    }

    func resume() {
        guard state == .paused, !models.isEmpty else { return }
        message = nil
        launch()
    }

    func approve() {
        guard state == .confirming, let action = pending, let page = pendingPage else { return }
        pending = nil
        pendingPage = nil
        launch(approved: action, page: page)
    }

    private func invalidate() { generation = UUID(); run?.cancel(); run = nil }

    private func launch(approved: BrowserDecision? = nil, page: BrowserPage? = nil) {
        invalidate()
        let id = generation
        state = .running
        browser.webView.endEditing(true)
        run = Task { [weak self] in await self?.execute(id: id, approved: approved, page: page) }
    }

    private func execute(id: UUID, approved: BrowserDecision?, page: BrowserPage?) async {
        do {
            try check(id)
            // The task composer dismisses its keyboard as this run starts.
            // Observe after the viewport has settled, including on resume.
            try await Task.sleep(for: .milliseconds(400))
            try check(id)
            if let approved {
                do { try await apply(approved, page: page) }
                catch BrowserToolError.stalePage { steps.append(.init(text: String(localized: "页面已变化，原操作没有执行。重新读取。"))) }
            }
            var staleRetries = 0
            for _ in 0..<30 {
                try check(id)
                try await browser.waitUntilReady()
                try check(id)
                let page = browser.hasPage ? try await browser.readPage() : nil
                let decision = try await reasoning.decide(goal: goal, page: page, steps: steps, models: models)
                try check(id)
                if decision.action == .done {
                    guard page != nil else { throw BrowserToolError.invalidResult }
                    browser.webView.endEditing(true)
                    state = .done
                    message = decision.explanation
                    return
                }
                if decision.action == .handoff {
                    state = .paused
                    message = decision.explanation
                    return
                }
                if needsApproval(decision, page: page), let page {
                    pending = decision
                    pendingPage = page
                    state = .confirming
                    return
                }
                do {
                    try await apply(decision, page: page)
                    staleRetries = 0
                } catch BrowserToolError.stalePage {
                    staleRetries += 1
                    guard staleRetries <= 3 else { throw BrowserToolError.stalePage }
                    steps.append(.init(text: String(localized: "页面已变化，操作没有执行。重新读取。")))
                } catch BrowserToolError.humanRequired {
                    state = .paused
                    message = BrowserToolError.humanRequired.localizedDescription
                    return
                }
            }
            state = .paused
            message = String(localized: "已执行 30 步，请检查网页后决定是否继续")
        } catch {
            guard generation == id, !Task.isCancelled else { return }
            state = .failed
            message = error.localizedDescription
        }
    }

    private func check(_ id: UUID) throws {
        guard generation == id, !Task.isCancelled else { throw CancellationError() }
    }

    private func needsApproval(_ decision: BrowserDecision, page: BrowserPage?) -> Bool {
        guard decision.action == .click else { return decision.requiresConfirmation == true }
        guard let control = page?.elements.first(where: { $0.id == decision.element }) else { return true }
        if control.isSubmit || decision.requiresConfirmation == true { return true }
        let label = control.label.lowercased()
        return ["支付", "付款", "购买", "下单", "删除", "发布", "发送", "提交", "同意", "pay", "buy", "purchase", "delete", "send", "submit", "publish", "agree"]
            .contains { label.contains($0) }
    }

    private func apply(_ decision: BrowserDecision, page: BrowserPage?) async throws {
        switch decision.action {
        case .open:
            guard let address = decision.url, let url = BrowserSession.destination(address),
                  ["http", "https"].contains(url.scheme ?? "") else { throw BrowserToolError.actionFailed(String(localized: "网页地址无效")) }
            browser.open(url.absoluteString)
        case .click, .fill, .select:
            guard let page, let element = decision.element, page.elements.contains(where: { $0.id == element }) else {
                throw BrowserToolError.stalePage
            }
            if decision.action == .click { try await browser.click(element: element, token: page.token) }
            else if decision.action == .select {
                guard let text = decision.text else { throw BrowserToolError.actionFailed(String(localized: "没有选择内容")) }
                try await browser.select(element: element, token: page.token, value: text)
            } else {
                guard let text = decision.text else { throw BrowserToolError.actionFailed(String(localized: "没有填写内容")) }
                try await browser.fill(element: element, token: page.token, text: text)
            }
        case .scroll:
            try await browser.scroll(direction: decision.text ?? "down")
        case .back:
            guard browser.webView.canGoBack else { throw BrowserToolError.actionFailed(String(localized: "没有上一页")) }
            browser.webView.goBack()
        case .wait: try await Task.sleep(for: .seconds(1))
        case .done, .handoff: return
        }
        steps.append(.init(text: decision.explanation, action: decision.action.rawValue))
        // Let event handlers and client-side rendering finish before observing again.
        try await Task.sleep(for: .milliseconds(350))
    }
}

extension BrowserSession {
    func waitUntilReady() async throws {
        for _ in 0..<400 {
            try Task.checkCancellation()
            if !loading {
                if let error { throw BrowserToolError.actionFailed(error) }
                return
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        throw BrowserToolError.actionFailed(String(localized: "网页加载超时，可以接手后重试"))
    }
}
