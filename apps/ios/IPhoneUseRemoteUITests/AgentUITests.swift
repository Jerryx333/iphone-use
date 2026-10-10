import XCTest

/// A goal typed on the Agent screen, carried out by `Tools/mock_daemon.py`
/// (skipped unless it answers): the goal pays, so the task stops for the
/// person's OK, the confirmation sheet comes up, and after "Allow" the
/// summary arrives. `TEST_RUNNER_SHOT_DIR=<dir>` saves screenshots there.
@MainActor
final class AgentUITests: XCTestCase {
    private let mock = URL(string: ProcessInfo.processInfo.environment["MOCK_DAEMON"] ?? "http://127.0.0.1:47001")!
    private var app: XCUIApplication!

    override func setUp() async throws {
        var request = URLRequest(url: mock.appendingPathComponent( "mock/info"))
        request.timeoutInterval = 3
        guard (try? await URLSession.shared.data(for: request)) != nil else {
            throw XCTSkip("mock daemon not running at \(mock)")
        }
    }

    override func tearDown() async throws {
        app?.terminate()
    }

    func testGoalConfirmAndSummary() throws {
        app = XCUIApplication()
        // `-agent YES` opens the Agent screen once the mock is added (and so on screen).
        app.launchArguments = ["-address", mock.absoluteString, "-password", "mock", "-observeOnly", "YES",
                               "-agent", "YES", "-legacyUI", "YES"]
        app.launch()

        let field = app.textFields["agent-goal"].exists ? app.textFields["agent-goal"] : app.textViews["agent-goal"]
        XCTAssertTrue(field.waitForExistence(timeout: 10), "no goal field")
        // The target connects in the background; wait until a send is possible.
        field.tap()
        field.typeText("打开拼多多付款")
        let send = app.buttons["agent-send"]
        let enabled = NSPredicate(format: "isEnabled == true")
        expectation(for: enabled, evaluatedWith: send)
        waitForExpectations(timeout: 15)
        send.tap()
        shot("agent-running")

        let approve = app.buttons["agent-approve"]
        XCTAssertTrue(approve.waitForExistence(timeout: 20), "the confirmation sheet did not come up")
        shot("agent-confirm")
        approve.tap()

        let summary = app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "87%")).firstMatch
        XCTAssertTrue(summary.waitForExistence(timeout: 20), "no summary after allowing the step")
        XCTAssertTrue(app.buttons["agent-save-flow"].waitForExistence(timeout: 5), "no Save as flow on a done task")
        shot("agent-done")
    }

    func testRefusedTaskKeepsTheDraft() throws {
        app = XCUIApplication()
        app.launchArguments = ["-address", mock.absoluteString, "-password", "mock", "-observeOnly", "YES",
                               "-agent", "YES", "-legacyUI", "YES"]
        app.launch()
        let field = app.textFields["agent-goal"].exists ? app.textFields["agent-goal"] : app.textViews["agent-goal"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText("__mock_refuse_task__")
        let send = app.buttons["agent-send"]
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        waitForExpectations(timeout: 15)
        send.tap()
        XCTAssertTrue(app.staticTexts["Mock task service is unavailable"].waitForExistence(timeout: 10))
        XCTAssertEqual(field.value as? String, "__mock_refuse_task__")
        XCTAssertTrue(send.isEnabled, "The person must be able to retry their draft")
        shot("agent-refused-draft")
        app.terminate()
        app.launch()
        let restored = app.textFields["agent-goal"].exists ? app.textFields["agent-goal"] : app.textViews["agent-goal"]
        XCTAssertTrue(restored.waitForExistence(timeout: 10))
        XCTAssertEqual(restored.value as? String, "__mock_refuse_task__")
    }

    private func shot(_ name: String) {
        let screenshot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
        if let dir = ProcessInfo.processInfo.environment["TEST_RUNNER_SHOT_DIR"] {
            try? screenshot.pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent( "\(name).png"))
        }
    }
}
