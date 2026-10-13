// Self Runner's host for the runner capability layer: the same sources as the device runner
// (runner/IPhoneUseRunner/IPhoneUseRunnerUITests, minus its XCTest shell RunnerTests.swift), built
// into RunnerCore.bundle and loaded only after the app has its own daemon session. XCTest is
// already in the process by then (SpikeBridge.loadXCTest), so this bundle binds to that copy.
//
// The app talks to this class by name only (NSClassFromString / perform), never by linking it.

import Foundation
import XCTest

@objc(IPURunnerCoreHost)
final class RunnerCoreHost: NSObject {
  private static var core: RunnerCore?
  private static var lastError: String?
  private static var shutdownRequested = false
  private static var uiTesting: [String: Any] = [:]

  /// XCUIAutomation refuses XCUIApplication (an assertion, fatal off a @try) unless the active
  /// XCTestConfiguration has initializeForUITesting (docs/research/2026-10-self-runner.md, E2).
  /// The runner's fallbacks and app routes construct XCUIApplication, so install one first.
  @objc(installUITestingConfiguration:)
  static func installUITestingConfiguration(_ sessionIdentifier: String) -> NSDictionary {
    if !uiTesting.isEmpty { return uiTesting as NSDictionary }
    var result: [String: Any] = [:]
    guard let cls = NSClassFromString("XCTestConfiguration") as? NSObject.Type else {
      result["error"] = "XCTestConfiguration missing"
      return result as NSDictionary
    }
    let setActive = NSSelectorFromString("setActiveTestConfiguration:")
    let configuration = cls.init()
    let failure = IPURBridge.catchException {
      configuration.setValue(true, forKey: "initializeForUITesting")
      if let uuid = UUID(uuidString: sessionIdentifier) {
        configuration.setValue(uuid as NSUUID, forKey: "sessionIdentifier")
      }
      _ = (cls as AnyObject).perform(setActive, with: configuration)
    }
    if let failure { result["error"] = failure }
    result["configuredForUITesting"] = (XCUIDevice.shared.value(forKey: "configuredForUITesting") as? Bool) ?? false
    uiTesting = result
    NSLog("selfrunner-core: UI testing configuration %@", String(describing: result))
    return result as NSDictionary
  }

  /// options: token (required), port, mjpegPort. Returns {ok, port, mjpegPort} or {ok:false, error}.
  @objc(start:)
  static func start(_ options: NSDictionary) -> NSDictionary {
    if let core { return ["ok": true, "port": Int(core.port), "already": true] }
    guard let token = options["token"] as? String, !token.isEmpty else {
      return ["ok": false, "error": "no runner token"]
    }
    var environment = ["IPU_RUNNER_TOKEN": token, "IPU_DAEMON_PORT": "0"]
    if let port = options["port"] as? NSNumber { environment["IPU_RUNNER_PORT"] = port.stringValue }
    if let port = options["mjpegPort"] as? NSNumber { environment["IPU_RUNNER_MJPEG_PORT"] = port.stringValue }
    let core = RunnerCore()
    core.onShutdown = {
      shutdownRequested = true
      NSLog("selfrunner-core: /shutdown received; stopping the runner API")
      stop()
    }
    core.onListenFailure = { error in
      lastError = "listen failed: \(error)"
      NSLog("selfrunner-core: %@", lastError!)
      stop()
    }
    do {
      try core.start(environment: environment)
    } catch {
      lastError = "start failed: \(error)"
      return ["ok": false, "error": lastError!]
    }
    self.core = core
    lastError = nil
    shutdownRequested = false
    return ["ok": true, "port": Int(core.port)]
  }

  @objc static func stop() {
    core?.stop()
    core = nil
  }

  @objc static func status() -> NSDictionary {
    var value: [String: Any] = ["running": core != nil, "shutdown_requested": shutdownRequested]
    if let core { value["port"] = Int(core.port) }
    if let lastError { value["error"] = lastError }
    if !uiTesting.isEmpty { value["ui_testing"] = uiTesting }
    return value as NSDictionary
  }
}
