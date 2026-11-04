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

  func testSystemScreenshotProbe() throws {
    serveEnded = true
    #if !targetEnvironment(simulator)
    throw XCTSkip("isolated Simulator capture diagnostic only")
    #else
    let env = ProcessInfo.processInfo.environment
    guard let runID = env["IPU_CAPTURE_PROBE_RUN_ID"], UUID(uuidString: runID) != nil else {
      throw XCTSkip("requires an explicit capture diagnostic run")
    }
    let app = XCUIApplication(bundleIdentifier: "com.leeguoo.iphone-use.capture-fixture")
    guard app.state == .runningForeground || app.state == .runningBackground else {
      XCTFail("owned capture fixture must already be running")
      return
    }
    if app.state != .runningForeground { app.activate() }
    guard app.state == .runningForeground,
          app.staticTexts["capture-marker"].waitForExistence(timeout: 5),
          app.staticTexts["capture-marker"].label == runID else {
      XCTFail("owned capture fixture is not in the foreground with this run's marker")
      return
    }
    let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    // Persist before dispatch. An infrastructure retry must never take another
    // system screenshot after an uncertain first attempt.
    try Data(runID.utf8).write(to: documents.appendingPathComponent("capture-attempt-\(runID)"), options: .withoutOverwriting)
    var result = IPURBridge.requestSystemScreenshot() as! [String: Any]
    result["probe_run_id"] = runID
    try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]).write(
      to: documents.appendingPathComponent("capture-dispatch.json"), options: .atomic)
    XCTAssertEqual(result["invoked"] as? Bool, true)
    XCTAssertEqual(result["acknowledged"] as? Bool, true)
    // The host independently checks the new asset's Photos database row and
    // original bytes. This App/Runner pair cannot declare persistence verified.
    #endif
  }

  func testServe() throws {
    let core = RunnerCore()
    self.core = core
    let expectation = XCTestExpectation(description: "ipu-runner serves until /shutdown")
    core.onShutdown = { expectation.fulfill() }
    core.onListenFailure = { _ in expectation.fulfill() }
    let environment = ProcessInfo.processInfo.environment
    try core.start(environment: environment)

    // The notification whose End button ends the session from the phone (RunnerExitNotice.swift).
    var exitNotice: RunnerExitNotice?
    if environment["IPU_RUNNER_EXIT_NOTICE"] != "0" {
      let notice = RunnerExitNotice {
        DispatchQueue.main.async { expectation.fulfill() }
      }
      notice.post(requestPermission: environment["IPU_RUNNER_EXIT_NOTICE"] == "1")
      exitNotice = notice
    }

    // Block this test (and keep the main run loop spinning for the request handlers) for a year,
    // or until POST /shutdown.
    let result = XCTWaiter.wait(for: [expectation], timeout: 365 * 24 * 60 * 60)
    NSLog("ipu-runner: serve loop ended (%@)", String(describing: result))
    exitNotice?.remove()
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
