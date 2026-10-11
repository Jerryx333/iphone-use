import XCTest
@testable import IPhoneUseRemote

private final class ZenCatalogFixture: URLProtocol, @unchecked Sendable {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        guard request.url?.absoluteString == "https://opencode.ai/zen/v1/models",
              request.value(forHTTPHeaderField: "Authorization") == "Bearer public" else {
            client?.urlProtocol(self, didFailWithError: URLError(.userAuthenticationRequired)); return
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: nil)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(#"{"data":[{"id":"big-pickle"}]}"#.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
}

@MainActor
final class ModelCatalogTests: XCTestCase {
    func testZenDiscoverySendsPublicToThePresetEndpoint() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ZenCatalogFixture.self]
        let models = try await ModelCatalog.fetch(ModelProvider(kind: .zen), session: URLSession(configuration: configuration))
        XCTAssertEqual(models.map(\.id), ["big-pickle"])
    }
    func testZenListExcludesPaidAndUnsupportedProtocols() throws {
        let data = Data(#"{"data":[{"id":"big-pickle"},{"id":"space-bunny-free"},{"id":"jev-1.13-free"},{"id":"muse-spark-1.3-contributor-free"},{"id":"gpt-paid"},{"id":"old-free","deprecated":true},{"id":"space-bunny-free"}]}"#.utf8)
        XCTAssertEqual(try ModelCatalog.parse(data, kind: .zen).map(\.id), ["big-pickle", "space-bunny-free"])
    }

    func testFreeListRequiresToolsAndPrioritizesImages() throws {
        let data = Data(#"{"data":[{"id":"text:free","supported_parameters":["tools"]},{"id":"image:free","supported_parameters":["tools"],"architecture":{"input_modalities":["text","image"]}},{"id":"paid","supported_parameters":["tools"]},{"id":"safety:free"},{"id":"no-tools:free","supported_parameters":["temperature"]}]}"#.utf8)
        let models = try ModelCatalog.parse(data, kind: .free)
        XCTAssertEqual(models.map(\.id), ["image:free", "text:free"])
        XCTAssertTrue(models[0].readsImages)
    }

    func testInferenceRestrictionIsReportedEvenWhenListingSucceeded() throws {
        let data = Data(#"{"type":"error","error":{"type":"FreeTierError","message":"OpenCode's free tier can only be used from within OpenCode"}}"#.utf8)
        let response = HTTPURLResponse(url: URL(string: "https://opencode.ai/zen/v1/chat/completions")!, statusCode: 403, httpVersion: nil, headerFields: nil)!
        XCTAssertThrowsError(try validateModelResponse(data, response: response)) { error in
            XCTAssertTrue(error.localizedDescription.contains("403"))
            XCTAssertTrue(error.localizedDescription.contains("Kilo"))
        }
    }
}
