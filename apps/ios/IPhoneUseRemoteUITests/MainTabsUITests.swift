import XCTest

@MainActor
final class MainTabsUITests: XCTestCase {
    private let fixture = ProcessInfo.processInfo.environment["BROWSER_FIXTURE"] ?? "http://127.0.0.1:47009/browser_fixture.html"

    override func setUp() async throws {
        var request = URLRequest(url: URL(string: fixture)!)
        request.timeoutInterval = 3
        guard let (data, _) = try? await URLSession.shared.data(for: request),
              String(data: data, encoding: .utf8)?.contains("Browser fixture") == true else {
            throw XCTSkip("Run python3 -m http.server 47009 --bind 127.0.0.1 --directory apps/ios/Tools")
        }
    }

    func testTabsAndBrowserNavigation() {
        let app = XCUIApplication()
        app.launchArguments = ["-tab", "browser", "-clearBrowserDraft", "YES", "-AppleLanguages", "(zh-Hans)"]
        app.launch()
        defer { app.terminate() }

        for title in ["设备", "浏览器", "任务", "设置"] {
            XCTAssertTrue(app.tabBars.buttons[title].waitForExistence(timeout: 10))
        }
        let field = app.textFields["browser-address"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        capture("browser-start")
        field.tap()
        field.typeText(fixture + "\n")
        XCTAssertTrue(app.webViews.firstMatch.waitForExistence(timeout: 15))
        XCTAssertTrue(app.webViews.staticTexts["Browser fixture"].waitForExistence(timeout: 30))
        capture("browser-loaded")
        app.buttons["browser-read"].tap()
        XCTAssertTrue(app.navigationBars["页面内容"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "Browser fixture")).firstMatch.exists)
        app.buttons["完成"].tap()
        app.tabBars.buttons["设置"].tap()
        XCTAssertTrue(app.staticTexts["模型与服务"].waitForExistence(timeout: 5))
        capture("settings")
        app.tabBars.buttons["任务"].tap()
        XCTAssertTrue(app.buttons["new-task"].waitForExistence(timeout: 5))
        capture("tasks")
        app.tabBars.buttons["浏览器"].tap()
        XCTAssertTrue(app.webViews.staticTexts["Browser fixture"].exists, "Switching tabs must preserve the page")
    }

    func testLegacyBrowserPromptAcceptsTextAndCancel() {
        let app = XCUIApplication()
        app.launchArguments = ["-tab", "browser", "-legacyUI", "YES", "-AppleLanguages", "(zh-Hans)"]
        app.launch()
        defer { app.terminate() }
        let address = app.textFields["browser-address"]
        XCTAssertTrue(address.waitForExistence(timeout: 10))
        address.tap()
        address.typeText(fixture + "\n")
        let prompt = app.webViews.buttons["Prompt fixture"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 20))
        prompt.tap()
        let input = app.textFields["browser-dialog-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        input.tap()
        input.typeText(" fixture")
        app.buttons["确认"].tap()
        XCTAssertTrue(app.webViews.staticTexts["initial fixture"].waitForExistence(timeout: 10))
        prompt.tap()
        XCTAssertTrue(input.waitForExistence(timeout: 10))
        app.buttons["取消"].tap()
        XCTAssertTrue(app.webViews.staticTexts["Prompt cancelled"].waitForExistence(timeout: 10))
    }

    func testLiveBrowserTaskFromTheComposer() throws {
        guard ProcessInfo.processInfo.environment["BROWSER_LIVE_MODEL"] == "1" else {
            throw XCTSkip("Set TEST_RUNNER_BROWSER_LIVE_MODEL=1 for the live browser task")
        }
        let app = XCUIApplication()
        app.launchArguments = ["-tab", "browser", "-clearBrowserDraft", "YES", "-AppleLanguages", "(zh-Hans)"]
        app.launch()
        defer { app.terminate() }
        let address = app.textFields["browser-address"]
        XCTAssertTrue(address.waitForExistence(timeout: 10))
        address.tap()
        address.typeText(fixture + "\n")
        XCTAssertTrue(app.webViews.staticTexts["Browser fixture"].waitForExistence(timeout: 20))
        let goal = app.textFields["browser-goal"].exists ? app.textFields["browser-goal"] : app.textViews["browser-goal"]
        goal.tap()
        goal.typeText("在 Name 中填写 Phone Use，点击 Continue，看到 Form updated 才完成。")
        let start = app.buttons["browser-start-task"]
        guard start.isEnabled else { XCTFail("The browser task composer must enable Send after typing a goal"); return }
        start.tap()
        capture("browser-task-running")
        let updated = app.webViews.staticTexts["Form updated"]
        let deadline = Date().addingTimeInterval(120)
        while Date() < deadline, !updated.exists {
            if app.buttons["browser-approve"].exists {
                capture("browser-task-confirm")
                app.buttons["browser-approve"].tap()
            }
            if app.buttons["browser-task-result"].exists { break }
            _ = updated.waitForExistence(timeout: 1)
        }
        XCTAssertTrue(updated.exists)
        let finishDeadline = Date().addingTimeInterval(90)
        while Date() < finishDeadline, !app.buttons["browser-task-result"].exists {
            if app.buttons["browser-approve"].exists {
                app.buttons["browser-approve"].tap()
            }
            _ = app.buttons["browser-task-result"].waitForExistence(timeout: 1)
        }
        XCTAssertTrue(app.buttons["browser-task-result"].exists)
        XCTAssertTrue(app.tabBars.buttons["设备"].isHittable, "Autofill must not leave the web keyboard toolbar over the tab bar")
        capture("browser-task-completed")
    }

    private func capture(_ name: String) {
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
