import XCTest
@testable import IPhoneUseRemote

@MainActor
final class ProviderMessageRequestTests: XCTestCase {
    func testBackgroundAndDuplicateCallbacksFinishOnlyOnce() async {
        let result = await ProviderMessageRequest.receive(timeoutNanoseconds: 1_000_000_000) { reply in
            DispatchQueue.global().async {
                reply(Data("first".utf8))
                reply(Data("duplicate".utf8))
            }
        }
        XCTAssertEqual(result, .response(Data("first".utf8)))
    }

    func testMissingCallbackTimesOutAndLateCallbackIsIgnored() async {
        var lateReply: (@Sendable (Data?) -> Void)?
        let result = await ProviderMessageRequest.receive(timeoutNanoseconds: 10_000_000) { lateReply = $0 }
        XCTAssertEqual(result, .timedOut)
        lateReply?(Data("late".utf8))
        lateReply?(nil)
    }

    func testCancellationBeforeDispatchDoesNotSend() async {
        var sent = false
        let request = Task {
            await ProviderMessageRequest.receive(timeoutNanoseconds: 1_000_000_000) { _ in sent = true }
        }
        request.cancel()
        let result = await request.value
        XCTAssertEqual(result, .cancelled)
        XCTAssertFalse(sent)
    }

    func testCancellationWhileWaitingIgnoresLaterReply() async {
        var lateReply: (@Sendable (Data?) -> Void)?
        let request = Task {
            await ProviderMessageRequest.receive(timeoutNanoseconds: 1_000_000_000) { lateReply = $0 }
        }
        while lateReply == nil { await Task.yield() }
        request.cancel()
        let result = await request.value
        XCTAssertEqual(result, .cancelled)
        lateReply?(Data("late".utf8))
    }

    func testSynchronousSendErrorReturnsFailure() async {
        let result = await ProviderMessageRequest.receive(timeoutNanoseconds: 1_000_000_000) { _ in
            throw NSError(domain: "synthetic-provider", code: 1)
        }
        XCTAssertEqual(result, .sendFailed)
    }

    func testNilReplyIsDifferentFromADeadline() async {
        let result = await ProviderMessageRequest.receive(timeoutNanoseconds: 1_000_000_000) { $0(nil) }
        XCTAssertEqual(result, .response(nil))
    }
}
