import XCTest
@testable import IPhoneUseRemote

@MainActor
final class FlowRecorderTests: XCTestCase {
    private let saved = FlowDraft(flow: Data("{\"steps\":[{}]}".utf8), steps: 1)

    func testLostStopReplyRecoversExactDraftWithoutRepeatingStop() async {
        var stops = 0
        let recorder = FlowRecorder(read: { [saved] in
            RecordingState(recording: false, steps: 0, draft: saved)
        }, start: {}, stop: {
            stops += 1
            throw URLError(.networkConnectionLost)
        })
        await recorder.stop()
        XCTAssertEqual(stops, 1)
        XCTAssertFalse(recorder.recording)
        XCTAssertTrue(recorder.statusKnown)
        XCTAssertEqual(recorder.draft, saved)
        recorder.leave()
    }

    func testUnreachableStopStaysUnknownThenReadRecovers() async {
        var reachable = false
        var stops = 0
        let recorder = FlowRecorder(read: { [saved] in
            guard reachable else { throw URLError(.notConnectedToInternet) }
            return RecordingState(recording: false, steps: 0, draft: saved)
        }, start: {}, stop: {
            stops += 1
            throw URLError(.networkConnectionLost)
        })
        await recorder.stop()
        XCTAssertFalse(recorder.statusKnown)
        reachable = true
        await recorder.toggle()
        XCTAssertEqual(recorder.draft, saved)
        XCTAssertEqual(stops, 1)
        XCTAssertTrue(recorder.statusKnown)
        recorder.leave()
    }

    func testStartDoesNotEraseExistingRemoteRecording() async {
        var starts = 0
        let recorder = FlowRecorder(read: { RecordingState(recording: true, steps: 7) },
                                    start: { starts += 1 }, stop: { nil })
        await recorder.start()
        XCTAssertEqual(starts, 0)
        XCTAssertTrue(recorder.recording)
        XCTAssertEqual(recorder.steps, 7)
        recorder.leave()
    }

    func testLostStartReplyReadsStateWithoutRepeatingStart() async {
        var starts = 0
        let recorder = FlowRecorder(read: { RecordingState(recording: starts > 0, steps: 0) },
                                    start: { starts += 1; throw URLError(.timedOut) }, stop: { nil })
        await recorder.start()
        XCTAssertEqual(starts, 1)
        XCTAssertTrue(recorder.recording)
        XCTAssertTrue(recorder.statusKnown)
        recorder.leave()
    }

    func testUnreachablePreflightDoesNotStart() async {
        var starts = 0
        let recorder = FlowRecorder(read: { throw URLError(.notConnectedToInternet) },
                                    start: { starts += 1 }, stop: { nil })
        await recorder.start()
        XCTAssertEqual(starts, 0)
        XCTAssertFalse(recorder.statusKnown)
        recorder.leave()
    }

    func testDismissedRecoveredDraftDoesNotReopenOnSync() async {
        let recorder = FlowRecorder(read: { [saved] in
            RecordingState(recording: false, steps: 0, draft: saved)
        }, start: {}, stop: { throw URLError(.timedOut) })
        await recorder.stop()
        recorder.draft = nil
        await recorder.sync()
        XCTAssertNil(recorder.draft)
        recorder.leave()
    }

    func testLatePollCannotUndoSuccessfulStop() async throws {
        var reads = 0
        var waiting: CheckedContinuation<RecordingState, Never>?
        let recorder = FlowRecorder(read: {
            reads += 1
            if reads == 1 { return RecordingState(recording: true, steps: 1) }
            return await withCheckedContinuation { waiting = $0 }
        }, start: {}, stop: { [saved] in saved }, pollIntervalNanoseconds: 1_000_000)
        await recorder.sync()
        for _ in 0..<100 where waiting == nil { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertNotNil(waiting, "the old poll must be in flight")
        await recorder.stop()
        waiting?.resume(returning: RecordingState(recording: true, steps: 99))
        try await Task.sleep(nanoseconds: 10_000_000)
        XCTAssertFalse(recorder.recording)
        XCTAssertTrue(recorder.statusKnown)
        XCTAssertEqual(recorder.steps, 1)
        XCTAssertEqual(recorder.draft, saved)
        recorder.leave()
    }

    func testPollFailureShowsUnknownAndLaterReadRecovers() async throws {
        var reads = 0
        let recorder = FlowRecorder(read: {
            reads += 1
            if reads == 2 { throw URLError(.networkConnectionLost) }
            return RecordingState(recording: true, steps: reads)
        }, start: {}, stop: { nil }, pollIntervalNanoseconds: 50_000_000)
        await recorder.sync()
        for _ in 0..<100 where reads < 2 { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertFalse(recorder.statusKnown)
        for _ in 0..<100 where reads < 3 { try await Task.sleep(nanoseconds: 1_000_000) }
        XCTAssertTrue(recorder.statusKnown)
        XCTAssertTrue(recorder.recording)
        recorder.leave()
    }
}
