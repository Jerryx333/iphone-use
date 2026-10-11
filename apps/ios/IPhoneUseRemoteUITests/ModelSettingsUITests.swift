import XCTest

@MainActor
final class ModelSettingsUITests: XCTestCase {
    func testPublicPresetAndSearchablePicker() {
        let app = XCUIApplication()
        app.launchArguments = ["-tab", "settings", "-observeOnly", "YES", "-modelSettingsFixture", "YES", "-AppleLanguages", "(zh-Hans)"]
        app.launch()
        XCTAssertTrue(app.buttons["模型与服务"].waitForExistence(timeout: 10))
        app.buttons["模型与服务"].tap()
        let zen = app.buttons.containing(NSPredicate(format: "label CONTAINS %@", "OpenCode Zen")).firstMatch
        XCTAssertTrue(zen.waitForExistence(timeout: 10))
        zen.tap()
        XCTAssertTrue(app.staticTexts["public"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.secureTextFields.firstMatch.exists, "The public preset does not ask for a personal key")
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "zen-public-preset"
        attachment.lifetime = .keepAlways
        add(attachment)
        app.buttons["从服务选择模型"].tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 5))
        let picker = XCTAttachment(screenshot: app.screenshot())
        picker.name = "model-search-picker"
        picker.lifetime = .keepAlways
        add(picker)
    }
}
