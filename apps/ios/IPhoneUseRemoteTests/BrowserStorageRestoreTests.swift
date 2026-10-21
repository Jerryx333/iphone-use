import XCTest
import WebKit
import JavaScriptCore
@testable import IPhoneUseRemote

@MainActor
final class BrowserStorageRestoreTests: XCTestCase {
    func testTargetNavigationRemovesSeedAndPreservesOtherScripts() throws {
        let controller = WKUserContentController()
        let other = WKUserScript(source: "window.fixture = true", injectionTime: .atDocumentEnd, forMainFrameOnly: true)
        controller.addUserScript(other)
        let restore = try BrowserStorageRestore(controller: controller, host: "example.com", values: ["sid": .string("synthetic")])
        XCTAssertEqual(controller.userScripts.count, 2)
        restore.completedNavigation(to: URL(string: "https://example.com/account")!)
        XCTAssertEqual(controller.userScripts.count, 1)
        XCTAssertEqual(controller.userScripts.first?.source, other.source)
        restore.completedNavigation(to: URL(string: "https://example.com/logout")!)
        XCTAssertEqual(controller.userScripts.count, 1)
    }

    func testOtherOriginsCannotConsumeRestore() throws {
        let controller = WKUserContentController()
        let restore = try BrowserStorageRestore(controller: controller, host: "example.com", values: ["sid": .string("synthetic")])
        for address in ["http://example.com", "https://example.com:8443", "https://other.test", "https://sub.example.com"] {
            restore.completedNavigation(to: URL(string: address)!)
            XCTAssertEqual(controller.userScripts.count, 1, address)
        }
        restore.completedNavigation(to: URL(string: "https://example.com:443/path")!)
        XCTAssertTrue(controller.userScripts.isEmpty)
    }

    func testScriptRestoresOnlyChosenOriginAndPreservesJSONValues() throws {
        let controller = WKUserContentController()
        let restore = try BrowserStorageRestore(controller: controller, host: "example.com", values: [
            "sid": .string("synthetic\"\\\nvalue"), "settings": .object(["enabled": .bool(true)]),
            "constructor": .string("skip"), "__proto__": .string("skip")
        ])
        let context = try XCTUnwrap(JSContext())
        context.evaluateScript("var location={origin:'https://outside.test'}; var result={}; var localStorage={setItem:(k,v)=>{result[k]=v;}};")
        let script = try XCTUnwrap(controller.userScripts.first?.source)
        context.evaluateScript(script)
        XCTAssertNil(context.exception)
        XCTAssertEqual(context.evaluateScript("Object.keys(result).length")?.toInt32(), 0)
        context.evaluateScript("location.origin='https://example.com';")
        context.evaluateScript(script)
        XCTAssertNil(context.exception)
        XCTAssertEqual(context.evaluateScript("result.sid")?.toString(), "synthetic\"\\\nvalue")
        XCTAssertEqual(context.evaluateScript("JSON.parse(result.settings).enabled")?.toBool(), true)
        XCTAssertEqual(context.evaluateScript("Object.prototype.hasOwnProperty.call(result,'constructor')")?.toBool(), false)
        restore.completedNavigation(to: URL(string: "https://example.com")!)
        context.evaluateScript("result.sid='new-synthetic-login';")
        for remaining in controller.userScripts { context.evaluateScript(remaining.source) }
        XCTAssertEqual(context.evaluateScript("result.sid")?.toString(), "new-synthetic-login")
    }
}
