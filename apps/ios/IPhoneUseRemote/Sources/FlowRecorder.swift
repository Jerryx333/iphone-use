import Combine
import Foundation

/// Mirrors daemon-owned recording. Unknown mutation outcomes are resolved by
/// reads, never by replaying a start (which would discard the current trail).
@MainActor
final class FlowRecorder: ObservableObject {
    @Published private(set) var recording = false
    @Published private(set) var steps = 0
    @Published private(set) var busy = false
    @Published private(set) var statusKnown = false
    @Published var draft: FlowDraft?
    @Published private(set) var pendingDraft: FlowDraft?

    private let read: () async throws -> RecordingState
    private let begin: () async throws -> Void
    private let end: () async throws -> FlowDraft?
    private let show: (String) -> Void
    private let pollIntervalNanoseconds: UInt64
    private var pollTask: Task<Void, Never>?
    private var generation = 0
    private var visible = true
    private var recoverStoppedDraft = false
    private var acknowledgedFlow: Data?

    init(read: @escaping () async throws -> RecordingState,
         start: @escaping () async throws -> Void,
         stop: @escaping () async throws -> FlowDraft?,
         pollIntervalNanoseconds: UInt64 = 2_000_000_000,
         show: @escaping (String) -> Void = { _ in }) {
        self.read = read; self.begin = start; self.end = stop; self.show = show
        self.pollIntervalNanoseconds = pollIntervalNanoseconds
    }

    func sync() async {
        visible = true
        guard !busy else { return }
        busy = true
        invalidatePoll()
        defer { busy = false; pollIfNeeded() }
        await refresh()
    }

    func toggle() async {
        if !statusKnown { await sync() }
        else if recording { await stop() }
        else if pendingDraft != nil { offerDraft() }
        else { await start() }
    }

    func start() async {
        guard !busy else { return }
        busy = true
        invalidatePoll()
        defer { busy = false; pollIfNeeded() }
        // A view's cached false state must not reset a recording started elsewhere.
        guard await refresh(), !recording else { return }
        guard pendingDraft == nil else { offerDraft(); return }
        do {
            try await begin()
            acknowledgedFlow = nil
            recoverStoppedDraft = false
            recording = true; steps = 0; statusKnown = true
            show(String(localized: "开始录制：接下来的操作会记成 flow"))
        } catch {
            statusKnown = false
            show(String(localized: "开始录制的结果未确认，正在检查；不会自动重试"))
            await refresh()
        }
    }

    func stop() async {
        guard !busy else { return }
        busy = true
        invalidatePoll()
        defer { busy = false; pollIfNeeded() }
        // If the response is lost, the daemon's status retains this exact draft.
        recoverStoppedDraft = true
        do {
            let stopped = try await end()
            recording = false; statusKnown = true
            accept(stopped)
            recoverStoppedDraft = false
        } catch {
            statusKnown = false
            show(String(localized: "停止录制的结果未确认，正在检查并恢复草稿"))
            await refresh()
        }
    }

    func leave() {
        visible = false
        invalidatePoll()
    }

    func offerDraft() { draft = pendingDraft }

    func finishDraft(_ saved: FlowDraft) {
        guard pendingDraft?.flow == saved.flow else { return }
        acknowledgedFlow = saved.flow
        pendingDraft = nil
    }

    func skipDraft() {
        acknowledgedFlow = pendingDraft?.flow
        pendingDraft = nil
        draft = nil
    }

    @discardableResult
    private func refresh() async -> Bool {
        do {
            let state = try await read()
            recording = state.recording; steps = state.steps; statusKnown = true
            if state.recording { acknowledgedFlow = nil }
            if !state.recording { remember(state.draft) }
            if recoverStoppedDraft, !state.recording {
                if let recovered = state.draft {
                    accept(recovered)
                    recoverStoppedDraft = false
                } else {
                    // Older daemons cannot retain a stopped draft. Do not claim
                    // that the recording was empty when its reply was lost.
                    show(String(localized: "录制已停止，但未能取回草稿；请检查手机助手版本"))
                    recoverStoppedDraft = false
                }
            }
            return true
        } catch {
            statusKnown = false
            return false
        }
    }

    private func accept(_ stopped: FlowDraft?) {
        if let stopped, stopped.flow != nil, stopped.steps > 0 {
            pendingDraft = stopped
            draft = stopped; steps = stopped.steps
        } else {
            show(String(localized: "这次没有录到步骤"))
        }
    }

    private func remember(_ stopped: FlowDraft?) {
        guard let stopped, let flow = stopped.flow, stopped.steps > 0,
              flow != acknowledgedFlow else { return }
        // A reopened remote view offers the draft without interrupting the
        // user with an automatic save sheet.
        if pendingDraft == nil { pendingDraft = stopped }
    }

    private func invalidatePoll() {
        generation += 1
        pollTask?.cancel(); pollTask = nil
    }

    private func pollIfNeeded() {
        guard visible, !busy, (recording || !statusKnown), pollTask == nil else { return }
        let epoch = generation
        let interval = pollIntervalNanoseconds
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: interval) }
                catch { break }
                guard let self, self.generation == epoch, !self.busy else { break }
                do {
                    let state = try await self.read()
                    guard !Task.isCancelled, self.generation == epoch else { break }
                    self.recording = state.recording; self.steps = state.steps; self.statusKnown = true
                    if state.recording { self.acknowledgedFlow = nil }
                    if !state.recording {
                        self.remember(state.draft)
                        if self.recoverStoppedDraft {
                            if let recovered = state.draft { self.accept(recovered) }
                            else { self.show(String(localized: "录制已停止，但未能取回草稿；请检查手机助手版本")) }
                            self.recoverStoppedDraft = false
                        }
                        break
                    }
                } catch {
                    guard !Task.isCancelled, self.generation == epoch else { break }
                    self.statusKnown = false
                }
            }
            if let self, self.generation == epoch { self.pollTask = nil }
        }
    }
}
