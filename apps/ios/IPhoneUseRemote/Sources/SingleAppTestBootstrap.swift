import Darwin
import Foundation

/// The regular SwiftUI app also hosts its embedded control test bundle when
/// its own on-device launcher supplies an authorized XCTest session.
/// Ordinary launches and the app's normal unit/UI tests never enter this path.
@MainActor
enum SingleAppTestBootstrap {
    private static var started = false
    private typealias TestMain = @convention(c) (UnsafeRawPointer?) -> Void

    enum Failure: Error { case missingRuntime, missingEntryPoint }

    static var requested: Bool {
        let environment = ProcessInfo.processInfo.environment
        guard Bundle.main.object(forInfoDictionaryKey: "IPUSingleAppControl") as? Bool == true,
              let session = environment["XCTestSessionIdentifier"], UUID(uuidString: session) != nil,
              let path = environment["XCTestBundlePath"] else { return false }
        let expected = Bundle.main.bundleURL.appendingPathComponent("PlugIns/PhoneUse.xctest").standardizedFileURL.path
        return URL(fileURLWithPath: path).standardizedFileURL.path == expected
    }

    static func startIfRequested() throws {
        guard requested, !started else { return }
        let paths = [Bundle.main.privateFrameworksURL?.appendingPathComponent("XCTest.framework/XCTest").path,
                     "/System/Developer/Library/Frameworks/XCTest.framework/XCTest"].compactMap { $0 }
        guard let handle = paths.lazy.compactMap({ dlopen($0, RTLD_NOW | RTLD_GLOBAL) }).first else {
            throw Failure.missingRuntime
        }
        guard let entry = dlsym(handle, "_XCTestMain") else { throw Failure.missingEntryPoint }
        let main = unsafeBitCast(entry, to: TestMain.self)
        started = true
        // Xcode's runner uses _XCTestMain(nil) when the environment specifies
        // the configuration. Keep the framework loaded for the session.
        // XCTest keeps this entry point alive. Running it inside a main-queue
        // dispatch block would occupy that serial queue forever, preventing
        // the runner's main-queue UI handlers from executing. Enter from a
        // run-loop block instead, so XCTest's nested run loop can drain them.
        RunLoop.main.perform(inModes: [.common]) { main(nil) }
    }
}
