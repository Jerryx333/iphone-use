import XCTest
@testable import IPhoneUseRemote

final class BrowserAccountScopeTests: XCTestCase {
    private func account() -> BrowserAccount {
        BrowserAccount(id: UUID(), externalID: "synthetic-account", label: "Fixture",
                       domains: ["example.com", "old.test"],
                       cookies: [BrowserCookie(name: "sid", value: "synthetic-example", domain: ".example.com"),
                                 BrowserCookie(name: "sid", value: "synthetic-old", domain: ".old.test")],
                       localStorage: ["token": .string("synthetic-old-origin")],
                       bookmarks: [], history: [], personal: [:],
                       lastURL: "https://old.test/profile", needsApply: false)
    }

    func testNewOriginCannotReceiveFormerStorageOrCookies() {
        var value = account()
        value.updateScope(["new.test"])
        XCTAssertTrue(value.needsApply)
        XCTAssertTrue(value.cookies.isEmpty)
        XCTAssertTrue(value.localStorage.isEmpty)
        XCTAssertNil(value.lastURL)
    }

    func testNarrowerScopeKeepsOnlyAuthorizedCookiesAndSameOriginStorage() {
        var value = account()
        value.updateScope(["example.com"])
        XCTAssertEqual(value.cookies.map(\.value), ["synthetic-example"])
        XCTAssertEqual(value.localStorage["token"], .string("synthetic-old-origin"))
        XCTAssertNil(value.lastURL)
        XCTAssertTrue(value.needsApply)
    }

    func testReorderedDomainsDoNotMoveStorageIntoAnotherOrigin() {
        var value = account()
        value.updateScope(["old.test", "example.com"])
        XCTAssertTrue(value.localStorage.isEmpty)
        XCTAssertEqual(value.cookies.count, 2)
        XCTAssertEqual(value.lastURL, "https://old.test/profile")
        XCTAssertTrue(value.needsApply)
    }

    func testUnchangedScopeDoesNotResetWebKitSession() {
        var value = account()
        value.updateScope(["example.com", "old.test"])
        XCTAssertFalse(value.needsApply)
        XCTAssertEqual(value.cookies.count, 2)
        XCTAssertFalse(value.localStorage.isEmpty)
    }

    func testHostSuffixCannotKeepAnotherWebsitesCookies() {
        var value = account()
        value.updateScope(["otherexample.com"])
        XCTAssertTrue(value.cookies.isEmpty)
        XCTAssertTrue(value.localStorage.isEmpty)
    }
}
