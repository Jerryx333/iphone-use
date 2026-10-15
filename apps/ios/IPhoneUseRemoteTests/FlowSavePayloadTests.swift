import XCTest
@testable import IPhoneUseRemote

final class FlowSavePayloadTests: XCTestCase {
    func testRecoveredDraftIsIncludedWithAllStepsAndInputs() throws {
        let flow = Data(#"{"version":1,"app":"com.apple.Preferences","steps":[{"kind":"type","input":"text1"}],"inputs":{"text1":{"required":true}}}"#.utf8)
        let encoded = try FlowSavePayload.encode(id: "settings/demo", name: "测试", description: "说明", flow: flow)
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        let sent = try XCTUnwrap(body["flow"] as? NSDictionary)
        XCTAssertEqual(sent, try JSONSerialization.jsonObject(with: flow) as? NSDictionary)
        XCTAssertEqual(body["name"] as? String, "测试")
        XCTAssertEqual(body["description"] as? String, "说明")
    }

    func testInvalidSuppliedDraftCannotBecomeAnImplicitSave() {
        for document in ["broken", "[]", "null", "7"] {
            XCTAssertThrowsError(try FlowSavePayload.encode(id: "settings/demo", name: "测试", description: nil,
                                                           flow: Data(document.utf8)))
        }
    }

    func testNilDraftExplicitlyUsesDaemonTrail() throws {
        let encoded = try FlowSavePayload.encode(id: "settings/demo", name: "测试", description: nil, flow: nil)
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        XCTAssertNil(body["flow"])
    }

    func testSuccessRequiresSavedIDMatchingRequest() throws {
        let result = Data(#"{"ok":true,"saved":{"id":"settings/demo","steps":3}}"#.utf8)
        XCTAssertEqual(try FlowSavePayload.savedID(from: result, expectedID: "settings/demo"), "settings/demo")
        XCTAssertThrowsError(try FlowSavePayload.savedID(from: result, expectedID: "settings/another"))
    }

    func testMissingOrInvalidReceiptCannotReportSuccess() {
        for response in ["{}", #"{"ok":true}"#, #"{"ok":false,"saved":{"id":"settings/demo"}}"#,
                         #"{"ok":true,"saved":null}"#, "broken"] {
            XCTAssertThrowsError(try FlowSavePayload.savedID(from: Data(response.utf8), expectedID: "settings/demo"))
        }
    }
}
