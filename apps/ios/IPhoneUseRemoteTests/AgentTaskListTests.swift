import XCTest
@testable import IPhoneUseRemote

final class AgentTaskListTests: XCTestCase {
    func testValidBareAndWrappedListsKeepRunningTasks() throws {
        for json in [#"[{"id":"t","status":"running"}]"#, #"{"ok":true,"tasks":[{"id":"t","status":"running"}]}"#] {
            let tasks = try AgentTaskList.decode(Data(json.utf8))
            XCTAssertEqual(tasks.count, 1)
            XCTAssertFalse(tasks[0].state.isTerminal)
        }
    }
    func testInvalidReplyCannotLookLikeAnEmptyTaskList() {
        for json in ["broken", "{}", #"{"tasks":null}"#, #"{"ok":false,"tasks":[]}"#,
                     #"{"tasks":[{"status":"running"}]}"#, #"[{"id":"done","status":"done"},{"status":"running"}]"#] {
            XCTAssertThrowsError(try AgentTaskList.decode(Data(json.utf8)), json)
        }
    }
    func testUnknownAndMissingTaskStatesRemainNonterminal() throws {
        let tasks = try AgentTaskList.decode(Data(#"{"tasks":[{"id":"first"},{"id":"second","status":"future_state"}]}"#.utf8))
        XCTAssertTrue(tasks.allSatisfy { !$0.state.isTerminal })
    }
    func testExplicitEmptyListIsValid() throws {
        XCTAssertTrue(try AgentTaskList.decode(Data(#"{"tasks":[]}"#.utf8)).isEmpty)
        XCTAssertTrue(try AgentTaskList.decode(Data("[]".utf8)).isEmpty)
    }
}
