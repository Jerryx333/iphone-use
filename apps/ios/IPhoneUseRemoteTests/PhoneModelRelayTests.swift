import XCTest
@testable import IPhoneUseRemote

@MainActor
final class PhoneModelRelayTests: XCTestCase {
    func testPhoneRouteKeepsSavedProviderPrivateAndRejectsWrongCredential() async throws {
        let remote = ModelsPayload.Assist(format: "openai", baseURL: "https://fixture.example/v1", key: "synthetic", model: "fixture")
        let original = ModelsPayload(assist: [remote])
        let relay = PhoneModelRelay()
        let converted = try await relay.payload(original)
        let routed = try XCTUnwrap(converted)
        XCTAssertEqual(original.assist[0], remote)
        XCTAssertEqual(routed.assist[0].model, remote.model)
        let url = try XCTUnwrap(URL(string: routed.assist[0].baseURL + "/chat/completions"))
        XCTAssertEqual(url.host, "127.0.0.1")
        XCTAssertFalse(url.absoluteString.contains("fixture.example"))
        var request = URLRequest(url: url)
        request.httpMethod = "POST"; request.httpBody = Data("{}".utf8)
        request.setValue("Bearer wrong-fixture", forHTTPHeaderField: "Authorization")
        request.timeoutInterval = 5
        do {
            _ = try await URLSession.shared.data(for: request)
            XCTFail("Unauthorized request reached an upstream model")
        } catch {
            XCTAssertTrue(error is URLError)
        }
    }
}
