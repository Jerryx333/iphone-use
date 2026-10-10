import Foundation
import Observation
import UIKit
import UserNotifications

/// The Agent screen's state: which device carries out the goal, the task
/// being followed, recent tasks there, and the question a paused task asks.
///
/// On 本机 the agent works in other apps while this one sits in the
/// background, where polling stops; the daemon brings this app back to the
/// front for a confirmation and at the end (`X-Task-Return-Bundle`), so the
/// task being followed is remembered and picked up again on every return.
@MainActor
@Observable
final class AgentController {
    @ObservationIgnored weak var app: AppModel?

    /// The device that gets the goal.
    var targetID: UUID? {
        didSet {
            guard targetID != oldValue else { return }
            history = []
            details = [:]
            error = nil
            Task { await refreshHistory() }
        }
    }

    /// `GET /agent/tasks` on the target, newest first, without steps.
    private(set) var history: [AgentTaskSnapshot] = []
    /// The task followed live, and the device it runs on.
    private(set) var live: AgentTaskTracker?
    private(set) var liveDeviceID: UUID?
    /// Steps of past tasks, read when the person opens one.
    private(set) var details: [String: AgentTaskSnapshot] = [:]
    /// A goal sent and not yet acknowledged.
    private(set) var pendingGoal: String?
    var error: String?
    /// Following the task gave up (the daemon stopped answering); it may
    /// still be running there.
    private(set) var linkNote: String?
    /// The question a paused task asks; drives the confirmation sheet.
    var confirmPrompt: ConfirmPrompt?
    private(set) var answering = false
    private(set) var stopping = false

    var settings: ModelSettings {
        didSet {
            guard settings != oldValue else { return }
            ModelSettingsStore.save(settings, to: defaults)
        }
    }

    struct ConfirmPrompt: Identifiable, Equatable {
        let taskID: String
        let deviceID: UUID
        let goal: String
        let confirm: AgentConfirm
        var id: String { "\(taskID)|\(confirm.action)|\(confirm.target ?? "")" }
    }

    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var backgroundTask: UIBackgroundTaskIdentifier = .invalid
    /// The confirmation already announced (sheet or notification), so a
    /// poll that repeats it does not announce it again.
    @ObservationIgnored private var announced: String?
    /// A question the person dismissed by answering: never shown again for
    /// the same task and step, even if a poll still reports it.
    @ObservationIgnored private var answered: Set<String> = []

    private static let runningKey = "agent.running.v1"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        settings = ModelSettingsStore.load(from: defaults)
    }

    // MARK: target

    var target: DeviceSession? { targetID.flatMap { app?.session($0) } }

    /// The device on screen, else 本机 when it is paired, else the first one.
    func chooseDefaultTarget() {
        guard let app else { return }
        if let focused = app.focusedID {
            targetID = focused
        } else if let own = app.devices.first(where: \.isThisPhone) {
            targetID = own.id
        } else if targetID == nil || app.session(targetID!) == nil {
            targetID = app.devices.first?.id
        }
    }

    /// The thread on the target, oldest first: past tasks (with their steps
    /// once read) and the live one.
    var thread: [AgentTaskSnapshot] {
        var items = history.reversed().map { item -> AgentTaskSnapshot in
            if let live, live.task.id == item.id, liveDeviceID == targetID { return live.task }
            return details[item.id] ?? item
        }
        if let live, liveDeviceID == targetID, !items.contains(where: { $0.id == live.task.id }) {
            items.append(live.task)
        }
        return items
    }

    /// A task is under way on the target (the composer becomes a Stop button).
    var busyOnTarget: Bool {
        guard let live, liveDeviceID == targetID else { return pendingGoal != nil }
        return !live.task.state.isTerminal || pendingGoal != nil
    }

    // MARK: running

    /// Hand `goal` to the target's daemon and follow it.
    func start(goal typed: String) async {
        let goal = typed.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !goal.isEmpty, pendingGoal == nil else { return }
        guard let session = target else {
            error = String(localized: "先选一台设备")
            return
        }
        guard let client = session.client, session.phase == .connected else {
            error = String(localized: "「\(session.name)」现在没有连上：\(session.shortState)")
            return
        }
        error = nil
        linkNote = nil
        pendingGoal = goal
        askNotificationPermission()
        let request = AgentTaskRequest(goal: goal, models: settings.payload(key: ModelSettingsStore.secret))
        let bundle = session.record.isThisPhone ? Bundle.main.bundleIdentifier : nil
        var id: String?
        for attempt in 0..<2 {
            do {
                id = try await client.startTask(request, returnBundle: bundle)
                break
            } catch DaemonError.sessionExpired where attempt == 0 {
                // Turned away before anything started: renew and send once more.
                if await session.relogin() { continue }
                error = DaemonError.sessionExpired.localizedDescription
            } catch AgentTaskError.taskRunning(let running) {
                // Already one there (or this POST's own, whose answer was lost):
                // show that one instead.
                id = running
                if running != live?.task.id { app?.show(String(localized: "那台手机上已经有一个任务在进行，正在显示它")) }
                break
            } catch {
                self.error = error.localizedDescription
            }
            break
        }
        pendingGoal = nil
        guard let id else { return }
        let first = AgentTaskSnapshot(id: id, goal: goal, state: .running)
        follow(AgentTaskTracker(first), on: session)
    }

    /// Follow `tracker` on `session` until it ends, remembering it so a
    /// relaunch picks it up again.
    private func follow(_ tracker: AgentTaskTracker, on session: DeviceSession) {
        guard let client = session.client else { return }
        pollTask?.cancel()
        live = tracker
        liveDeviceID = session.id
        linkNote = nil
        remember(task: tracker.task.id, device: session.id)
        holdBackgroundTime()
        let taskID = tracker.task.id
        pollTask = Task { [weak self, weak session] in
            let end = await AgentTaskPoller.follow(
                tracker, api: client,
                relogin: { await session?.relogin() ?? false },
                onUpdate: { self?.received($0) })
            self?.ended(end, taskID: taskID)
        }
    }

    private func received(_ tracker: AgentTaskTracker) {
        let previous = live?.task.state
        live = tracker
        let task = tracker.task
        if let confirm = task.pendingConfirm, let deviceID = liveDeviceID {
            let prompt = ConfirmPrompt(taskID: task.id, deviceID: deviceID, goal: task.goal, confirm: confirm)
            if !answered.contains(prompt.id), announced != prompt.id {
                announced = prompt.id
                confirmPrompt = prompt
                // Wherever the person is in the app, the question comes up.
                app?.agentOpen = true
                notifyIfAway(title: String(localized: "Agent 等你确认"),
                             body: Self.describe(confirm), id: "agent-confirm-\(task.id)")
            }
        } else if confirmPrompt?.taskID == task.id {
            confirmPrompt = nil
        }
        if task.state.isTerminal, previous.map({ !$0.isTerminal }) ?? true {
            notifyIfAway(title: task.state.title, body: task.summary ?? task.error ?? task.goal,
                         id: "agent-end-\(task.id)")
        }
    }

    private func ended(_ end: AgentPollEnd, taskID: String) {
        guard live?.task.id == taskID else { return }
        switch end {
        case .finished:
            forgetRunning()
            Task { await refreshHistory() }
        case .lostLink(let message):
            linkNote = message
        case .stopped:
            break
        }
        pollTask = nil
        releaseBackgroundTime()
    }

    /// Stop: the daemon stops after the current step; following goes on
    /// until it reports `cancelled`.
    func stop() async {
        guard let live, let deviceID = liveDeviceID, let client = app?.session(deviceID)?.client else { return }
        stopping = true
        defer { stopping = false }
        do {
            try await client.cancelTask(id: live.task.id)
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// The person's answer to a paused task. Nothing ever answers for them.
    func answer(_ prompt: ConfirmPrompt, approve: Bool) async {
        guard let client = app?.session(prompt.deviceID)?.client else {
            error = String(localized: "设备没有连上，回答没有送出")
            return
        }
        answering = true
        defer { answering = false }
        do {
            try await client.answer(task: prompt.taskID, approve: approve)
            answered.insert(prompt.id)
            if confirmPrompt == prompt { confirmPrompt = nil }
            UNUserNotificationCenter.current().removeDeliveredNotifications(
                withIdentifiers: ["agent-confirm-\(prompt.taskID)"])
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Bring the question of the live task back up (the person closed the screen).
    func reopenConfirm() {
        guard let live, let deviceID = liveDeviceID, let confirm = live.task.pendingConfirm else { return }
        confirmPrompt = ConfirmPrompt(taskID: live.task.id, deviceID: deviceID, goal: live.task.goal, confirm: confirm)
    }

    // MARK: history

    func refreshHistory() async {
        guard let session = target, let client = session.client else { return }
        let id = session.id
        do {
            let list = try await client.recentTasks()
            guard targetID == id else { return }
            history = list
        } catch DaemonError.sessionExpired {
            if await session.relogin(), targetID == id, let list = try? await client.recentTasks() { history = list }
        } catch {
            // No history (an older daemon) is not worth a message.
        }
    }

    /// Read a past task's steps when the person opens it.
    func loadDetail(_ id: String) async {
        guard details[id] == nil, live?.task.id != id, let client = target?.client else { return }
        if let task = try? await client.task(id: id, since: 0) { details[id] = task }
    }

    // MARK: coming back

    /// The app is in front again (or just launched): pick the remembered
    /// task up where it is, so a pending question shows at once.
    func resume() {
        guard pollTask == nil, let saved = defaults.string(forKey: Self.runningKey) else {
            if pollTask != nil { holdBackgroundTime() }
            return
        }
        let parts = saved.split(separator: "|", maxSplits: 1).map(String.init)
        guard parts.count == 2, let deviceID = UUID(uuidString: parts[0]) else {
            forgetRunning()
            return
        }
        let taskID = parts[1]
        pollTask = Task { [weak self] in
            // At launch the device is still connecting: wait for it a little.
            for _ in 0..<20 {
                guard let self else { return }
                if let session = self.app?.session(deviceID), session.client != nil, session.phase == .connected {
                    self.pollTask = nil
                    let seed = self.live?.task.id == taskID
                        ? self.live! : AgentTaskTracker(AgentTaskSnapshot(id: taskID, goal: "", state: .running))
                    if self.targetID == nil { self.targetID = deviceID }
                    self.follow(seed, on: session)
                    return
                }
                if self.app?.session(deviceID) == nil {
                    self.forgetRunning()
                    self.pollTask = nil
                    return
                }
                try? await Task.sleep(for: .seconds(1))
            }
            self?.pollTask = nil
        }
    }

    private func remember(task id: String, device: UUID) {
        defaults.set("\(device.uuidString)|\(id)", forKey: Self.runningKey)
    }

    private func forgetRunning() {
        defaults.removeObject(forKey: Self.runningKey)
    }

    /// A device was forgotten: drop what was followed there.
    func forget(device id: UUID) {
        if liveDeviceID == id {
            pollTask?.cancel()
            pollTask = nil
            live = nil
            liveDeviceID = nil
            forgetRunning()
        }
        if targetID == id { targetID = nil }
    }

    // MARK: background and notifications

    /// A few more seconds of polling after the person leaves the app (iOS
    /// gives about 30 s), so a question or the end can still be announced.
    private func holdBackgroundTime() {
        guard backgroundTask == .invalid else { return }
        backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "agent-task") { [weak self] in
            MainActor.assumeIsolated { self?.releaseBackgroundTime() }
        }
    }

    private func releaseBackgroundTime() {
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    /// Asked the first time a task starts, not before: a person who never
    /// uses the agent is never asked.
    private func askNotificationPermission() {
        Task {
            let center = UNUserNotificationCenter.current()
            let settings = await center.notificationSettings()
            if settings.authorizationStatus == .notDetermined {
                _ = try? await center.requestAuthorization(options: [.alert, .sound])
            }
        }
    }

    private func notifyIfAway(title: String, body: String, id: String) {
        guard UIApplication.shared.applicationState != .active else { return }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        let request = UNNotificationRequest(identifier: id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    /// `CLICK「确认支付」· 拼多多 — pays ¥55.51`
    static func describe(_ confirm: AgentConfirm) -> String {
        var text = confirm.action
        if let target = confirm.target, !target.isEmpty { text += "「\(target)」" }
        if let app = confirm.app, !app.isEmpty { text += " · \(app)" }
        if let reason = confirm.reason, !reason.isEmpty { text += " — \(reason)" }
        return text
    }
}
