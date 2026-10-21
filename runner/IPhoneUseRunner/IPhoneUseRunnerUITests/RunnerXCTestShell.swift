// XCTest shell of the device runner: RunnerTests.testServe (the method setup-runner.sh launches)
// starts RunnerCore and blocks in XCTWaiter. The capabilities are RunnerCore (RunnerTests.swift:
// the file keeps its old name so upstream edits to it still merge), which Self Runner also hosts.

import Network
import XCTest

final class RunnerTests: XCTestCase {
  var core: RunnerCore?
  /// Once serving ends, listener failures must reach xcodebuild as the test's own verdict.
  var serveEnded = false

  override func setUp() {
    continueAfterFailure = true
    // Only enforced when xcodebuild runs with -test-timeouts-enabled YES; keep it out of reach
    // anyway so a timeout-enabled launch does not kill the server after the default 10 minutes.
    executionTimeAllowance = 365 * 24 * 60 * 60
  }

  override func record(_ issue: XCTIssue) {
    if serveEnded || core == nil {
      super.record(issue)
      return
    }
    core?.noteIssue(issue.compactDescription)
  }

  func testServe() throws {
    let core = RunnerCore()
    self.core = core
    let expectation = XCTestExpectation(description: "ipu-runner serves until /shutdown")
    core.onShutdown = { expectation.fulfill() }
    core.onListenFailure = { _ in expectation.fulfill() }
    try core.start(environment: ProcessInfo.processInfo.environment)

    // Block this test (and keep the main run loop spinning for the request handlers) for a year,
    // or until POST /shutdown.
    let result = XCTWaiter.wait(for: [expectation], timeout: 365 * 24 * 60 * 60)
    NSLog("ipu-runner: serve loop ended (%@)", String(describing: result))
    core.stop()
    // A runner that never served must not end as a passing test: xcodebuild would print
    // "TEST EXECUTE SUCCEEDED" for a phone nobody can drive. The usual cause is another runner
    // still holding the port on the phone.
    if let failure = core.listenFailure {
      let busy = (failure as? NWError).map { error -> Bool in
        if case .posix(let code) = error { return code == .EADDRINUSE }
        return false
      } ?? false
      let message = busy
        ? "ipu-runner: port \(core.port) on the phone is already in use — another runner (or a previous one still exiting) holds it"
        : "ipu-runner: could not listen on port \(core.port): \(failure)"
      NSLog("%@", message)
      serveEnded = true
      XCTFail(message)
    }
  }
}
