import XCTest
@testable import IPhoneUseRemote

final class ZenStreamTests: XCTestCase {
    private let complete = #"data: {"choices":[{"delta":{"tool_calls":[{"id":"fixture_1","type":"function","function":{"name":"fixture_result","arguments":"{\"ok\":true}"}}]},"finish_reason":"tool_calls"}]}"#

    private func arguments(_ result: [String: Any]) -> String? {
        let choices = result["choices"] as? [[String: Any]]
        let message = choices?.first?["message"] as? [String: Any]
        let calls = message?["tool_calls"] as? [[String: Any]]
        return (calls?.first?["function"] as? [String: String])?["arguments"]
    }

    func testCompleteTerminalToolCallWithoutIndex() throws {
        let stream = complete + "\ndata: {\"choices\":[],\"usage\":{\"total_tokens\":12}}\ndata: [DONE]\n"
        XCTAssertEqual(arguments(try ZenCompatibility.collapse(Data(stream.utf8))), #"{"ok":true}"#)
        XCTAssertThrowsError(try ZenCompatibility.collapse(Data(complete.utf8)))
    }

    func testUnindexedIncompleteCallsCannotBeGuessed() {
        let broken = [
            complete.replacingOccurrences(of: #""finish_reason":"tool_calls""#, with: #""finish_reason":null"#),
            complete.replacingOccurrences(of: #""id":"fixture_1""#, with: #""id":"""#),
            complete.replacingOccurrences(of: #""type":"function""#, with: #""type":"unknown""#),
            complete.replacingOccurrences(of: #"{\"ok\":true}"#, with: #"{\"ok\":"#)
        ]
        for stream in broken {
            XCTAssertThrowsError(try ZenCompatibility.collapse(Data((stream + "\ndata: [DONE]\n").utf8)))
        }
    }

    func testCompleteCallCannotReplaceAnotherIndexedCall() {
        let fragment = #"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"another_call","function":{"name":"fixture_result","arguments":"{"}}]}}]}"#
        XCTAssertThrowsError(try ZenCompatibility.collapse(Data((fragment + "\n" + complete + "\ndata: [DONE]\n").utf8)))
    }

    func testCompleteCallFinishesMatchingIndexedFragments() throws {
        let fragment = #"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"fixture_1","function":{"name":"fixture_result","arguments":"{"}}]}}]}"#
        let result = try ZenCompatibility.collapse(Data((fragment + "\n" + complete + "\ndata: [DONE]\n").utf8))
        XCTAssertEqual(arguments(result), #"{"ok":true}"#)
    }

    func testIndexedFragmentsStillRequireBothFinishAndDone() throws {
        let first = #"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"fixture_1","function":{"name":"fixture_result","arguments":"{\"ok\":"}}]}}]}"#
        let last = #"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"true}"}}]},"finish_reason":"tool_calls"}]}"#
        let stream = first + "\n" + last + "\ndata: [DONE]\n"
        XCTAssertEqual(arguments(try ZenCompatibility.collapse(Data(stream.utf8))), #"{"ok":true}"#)
        XCTAssertThrowsError(try ZenCompatibility.collapse(Data((first + "\ndata: [DONE]\n").utf8)))
        XCTAssertThrowsError(try ZenCompatibility.collapse(Data((first + "\n" + last).utf8)))
    }
}
