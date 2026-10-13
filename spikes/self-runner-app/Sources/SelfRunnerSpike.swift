import SwiftUI
import CoreLocation
import Darwin
import CoreFoundation

@main
struct SelfRunnerSpikeApp: App {
    @StateObject private var model = RunnerModel()
    var body: some Scene { WindowGroup { ContentView(model: model) } }
}

// C handles use the boot worker; AX, screenshot and synthesis have independent serial workers.
final class RunnerModel: NSObject, ObservableObject, CLLocationManagerDelegate {
    @Published var state = "idle"
    @Published var ready = false { didSet { remoteLink.sessionChanged() } }
    private var needsPasscode = false // Main only; a timeout is a prompt hint, not proof.
    private var needsRelaunch = false
    private var autoSessionAttempted = false // Process lifetime, independent of network epochs.
    private var processSessionIdentifier: String? // Reuse the singleton's original IDE session identity.
    private var automationInFlight = false
    private var automationTimedOut = false
    private let automationWorker = DispatchQueue(label: "dev.iphoneuse.selfrunner.automation")
    @Published var busy = false
    @Published var logs = ""
    @Published var image: UIImage?
    @Published var hits = 0
    @Published var maxDepth = 24
    @Published var authorizePID = false
    @Published private(set) var axInFlight = false
    @Published private(set) var axLoadAttempted = false // Process lifetime; Stop does not reset this.
    @Published var autoSession = false {
        didSet {
            UserDefaults.standard.set(autoSession, forKey: "autoSession")
            configureAutoSession()
        }
    }
    @Published var autoMount = false {
        didSet { UserDefaults.standard.set(autoMount, forKey: "autoMount") }
    }
    @Published var remoteMode = false {
        didSet {
            UserDefaults.standard.set(remoteMode, forKey: "remoteMode")
            remoteLink.configure(enabled: remoteMode, pairing: remotePairing)
            if remoteMode { updateLocation() }
        }
    }
    @Published var keepLocation = true
    // The device runner's 8100/9100 API served from this process (RunnerCore.bundle), once ready.
    @Published var runnerCore = false {
        didSet {
            UserDefaults.standard.set(runnerCore, forKey: "runnerCore")
            if runnerCore { startRunnerCore() } else { stopRunnerCore() }
        }
    }
    private var runnerCoreHost: AnyObject?
    @Published var interfaceIP = UserDefaults.standard.string(forKey: "rpInterfaceIP") ?? "10.7.1.1"
    @Published var peerIP = UserDefaults.standard.string(forKey: "rpPeerIP") ?? "10.7.0.1"
    @Published var automaticPort = UserDefaults.standard.object(forKey: "rpAutomaticPort") as? Bool ?? true
    @Published var manualPort = UserDefaults.standard.string(forKey: "rpManualPort") ?? "49152"
    weak var target: UIButton?
    private let worker = DispatchQueue(label: "dev.iphoneuse.selfrunner.boot")
    private let axWorker = DispatchQueue(label: "dev.iphoneuse.selfrunner.ax")
    private let screenshotWorker = DispatchQueue(label: "dev.iphoneuse.selfrunner.screenshot")
    private let tapWorker = DispatchQueue(label: "dev.iphoneuse.selfrunner.tap")
    // Operation bookkeeping is main-only. A timed-out call is still running, not cancelled.
    private final class Operation {
        let name: String
        let generation: Int
        let started = Date()
        var step = "queued"
        var completed = false
        var timedOut = false
        let commandID: UUID?
        init(_ name: String, generation: Int, commandID: UUID?) {
            self.name = name; self.generation = generation; self.commandID = commandID
        }
    }
    private var activeOperation: Operation?
    private let remoteLink = RemoteLink()
    private let remoteActions = RemoteActions()
    private let remoteWorker = DispatchQueue(label: "dev.iphoneuse.selfrunner.remote.actions")
    private var remotePairing: RemotePairing?
    private var remoteCache: [String: [String: Any]] = [:]
    private var remoteOrder: [String] = []
    private var remoteInFlightID: String?
    private var lastActionOKAt: Double?
    private var activeObserver: NSObjectProtocol?
    private var handle: OpaquePointer? // worker only
    private var workerToken = 0
    private let location = CLLocationManager()
    private let discovery = RPDiscovery()
    private var timer: Timer?
    private var generation = 0
    private var stopping = false
    private let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    private var mailbox: ResearchMailbox?
    private var mailboxTimer: Timer?
    private var dispatchingCommandID: UUID?
    private var startCommandID: UUID?
    private var observeDisconnect: Bool { autoSession || ProcessInfo.processInfo.arguments.contains("--observe-disconnect") }
    private var autoTimer: Timer?
    private var autoEpoch = 0
    private var autoProbeInFlight = false
    private var autoNextProbe = Date.distantPast
    private var autoWaitLogged = Date.distantPast
    private var autoNextCheck = Date.distantPast
    private var autoChecking = false
    private var autoMountFailed = false // Main only: a failed attempt requires a manual Start.
    private var autoRetryAfterFailure = false
    private var autoChecks = 0
    private var lastAutoCheck: [String: Any] = [:]
    private var lastAutoScreenshot: [String: Any] = [:]
    private var axLoadVariant: String?
    private var axLoadSuccess: Bool?

    // Disconnect observation is main-only, like operation bookkeeping.
    private var bootDisconnected = false
    private var bootDisconnectedAt: Double = 0
    private var bootState = "unknown"

    override init() {
        super.init()
        let arguments = ProcessInfo.processInfo.arguments
        // If both overrides are supplied, the last one wins.
        for argument in arguments {
            if argument == "--remote-on" { UserDefaults.standard.set(true, forKey: "remoteMode") }
            if argument == "--remote-off" { UserDefaults.standard.set(false, forKey: "remoteMode") }
            if argument == "--auto-session-on" { UserDefaults.standard.set(true, forKey: "autoSession") }
            if argument == "--auto-session-off" { UserDefaults.standard.set(false, forKey: "autoSession") }
            if argument == "--auto-mount-on" { UserDefaults.standard.set(true, forKey: "autoMount") }
            if argument == "--auto-mount-off" { UserDefaults.standard.set(false, forKey: "autoMount") }
            if argument == "--runner-core-on" { UserDefaults.standard.set(true, forKey: "runnerCore") }
            if argument == "--runner-core-off" { UserDefaults.standard.set(false, forKey: "runnerCore") }
        }
        autoMount = UserDefaults.standard.bool(forKey: "autoMount")
        autoSession = UserDefaults.standard.bool(forKey: "autoSession")
        runnerCore = UserDefaults.standard.bool(forKey: "runnerCore")
        location.delegate = self
        location.desiredAccuracy = kCLLocationAccuracyThreeKilometers
        location.distanceFilter = kCLDistanceFilterNone
        location.pausesLocationUpdatesAutomatically = false
        log("SelfRunnerSpike: idle; mount DDI and enable LocalDevVPN before Start")
        log("observe disconnect mode: \(observeDisconnect ? "enabled" : "disabled")")
        // Location permission is requested on Start, after the UI explains its purpose.
        if ProcessInfo.processInfo.arguments.contains("--research-mailbox") {
            mailbox = ResearchMailbox(documents: documents)
            log("research USB mailbox enabled; runID=\(mailbox!.runID.uuidString); no network listener")
            pollMailbox()
            mailboxTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.pollMailbox() }
        }
        remoteLink.ready = { [weak self] in self?.ready ?? false }
        remoteLink.request = { [weak self] frame, reply in self?.executeRemote(frame, reply: reply) }
        importRemotePairing()
        remoteMode = UserDefaults.standard.bool(forKey: "remoteMode")
        remoteLink.configure(enabled: remoteMode, pairing: remotePairing)
        if remoteMode { updateLocation() }
        activeObserver = NotificationCenter.default.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            self?.remoteLink.reconnectNow()
        }
        configureAutoSession()
    }
    private func importRemotePairing() {
        do { remotePairing = try RemotePairing.loadOrImport(documents: documents) }
        catch RemotePairing.Failure.code(let code) { log("pairing: \(code)") }
        catch { log("pairing: import_failed") }
    }
    func rePair() {
        remoteLink.configure(enabled: false, pairing: nil)
        do {
            try RemotePairing.clear(); remotePairing = nil
            importRemotePairing()
        } catch RemotePairing.Failure.code(let code) { log("pairing: \(code)") }
        catch { log("pairing: reset_failed") }
        remoteLink.configure(enabled: remoteMode, pairing: remotePairing)
    }
    private func executeRemote(_ frame: [String: Any], reply: @escaping ([String: Any]) -> Void) {
        guard let id = frame["id"] as? String, !id.isEmpty, id.utf8.count <= 128,
              id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 95, 58].contains($0) }),
              let action = frame["action"] as? String else { return }
        if let cached = remoteCache[id] { reply(cached); return }
        let started = ProcessInfo.processInfo.systemUptime
        let safeAction = ["status", "read", "screenshot", "tap", "type", "swipe", "press", "open_url", "launch", "wait_for", "unlock"].contains(action) ? action : "unsupported"
        let finish: (RemoteActionResult, Bool) -> Void = { outcome, cache in
            let ms = Int((ProcessInfo.processInfo.systemUptime - started) * 1000)
            let response: [String: Any] = ["t": "res", "id": id, "ok": outcome.code == "ok",
                "result": outcome.result as Any? ?? NSNull(),
                "error": outcome.code == "ok" ? NSNull() : ["code": outcome.code, "message": outcome.code] as Any, "ms": ms]
            if cache {
                self.remoteCache[id] = response
                self.remoteOrder.removeAll { $0 == id }; self.remoteOrder.append(id)
                if self.remoteOrder.count > 64 { self.remoteCache.removeValue(forKey: self.remoteOrder.removeFirst()) }
            }
            if outcome.code == "ok" { self.lastActionOKAt = Date().timeIntervalSince1970 }
            self.log("remote action=\(safeAction) id=\(id) ms=\(ms) code=\(outcome.code)")
            reply(response)
        }
        if action != "status", busy || activeOperation != nil {
            finish(RemoteActionResult(code: "busy"), self.remoteInFlightID != id); return
        }
        guard let deadline = frame["deadline_ms"] as? NSNumber, CFGetTypeID(deadline) != CFBooleanGetTypeID(),
              deadline.doubleValue.isFinite, deadline.doubleValue >= 0,
              frame["args"] == nil || frame["args"] is NSNull || frame["args"] is [String: Any] else { finish(RemoteActionResult(code: "invalid_args"), true); return }
        let args = frame["args"] as? [String: Any] ?? [:]
        if action == "type", let text = args["text"] as? String { log("remote action=type text_length=\(text.utf16.count)") }
        let expires = started + deadline.doubleValue / 1000
        guard ProcessInfo.processInfo.systemUptime < expires else { finish(RemoteActionResult(code: "expired"), true); return }
        needsRelaunch = needsRelaunch || SpikeBridge.needsRelaunch()
        if needsRelaunch { ready = false }
        // Bootstrap/enable can be blocked in private code. Status must still report the hint.
        if action == "status", busy || activeOperation != nil || !ready {
            let initialStatus = sessionStatus()
            let address = interfaceIP
            remoteWorker.async {
                var status = initialStatus
                status["foreground"] = ["pid": 0, "bundle": "unknown"]
                let json = ipu_boot_list_interfaces_json().map { String(cString: $0) }
                let entries = json.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [[String: Any]] }
                status["vpn"] = entries.map { $0.contains { ($0["name"] as? String)?.hasPrefix("utun") == true && ($0["address"] as? String) == address } } as Any? ?? NSNull()
                let outcome = RemoteActionResult(status)
                DispatchQueue.main.async { finish(outcome, true) }
            }
            return
        }
        guard !busy, activeOperation == nil else { finish(RemoteActionResult(code: "busy"), self.remoteInFlightID != id); return }
        guard ["status", "read", "screenshot", "tap", "type", "swipe", "press", "launch", "open_url", "wait_for", "unlock"].contains(action) else { finish(RemoteActionResult(code: "unsupported"), true); return }
        guard action == "status" || ready else { finish(RemoteActionResult(code: "not_ready"), true); return }
        guard let op = beginOperation("remote \(safeAction)", requiresReady: false) else { finish(RemoteActionResult(code: "busy"), self.remoteInFlightID != id); return }
        remoteInFlightID = id
        DispatchQueue.main.asyncAfter(deadline: .now() + 20) {
            guard !op.completed else { return }
            self.log("remote action=\(safeAction) id=\(id) ms=\(Int((ProcessInfo.processInfo.systemUptime - started) * 1000)) code=still_running")
        }
        let portrait = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first?.interfaceOrientation == .portrait
        let depth = maxDepth
        let vpnAddress = interfaceIP
        let status = sessionStatus()
        remoteWorker.async {
            let json = ipu_boot_list_interfaces_json().map { String(cString: $0) }
            let entries = json.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [[String: Any]] }
            var status = status
            status["vpn"] = entries.map { $0.contains { ($0["name"] as? String)?.hasPrefix("utun") == true && ($0["address"] as? String) == vpnAddress } } as Any? ?? NSNull()
            let result = self.remoteActions.execute(action: action, args: args, depth: depth, portrait: portrait, status: status,
                expired: { ProcessInfo.processInfo.systemUptime >= expires })
            DispatchQueue.main.async {
                op.completed = true
                self.remoteInFlightID = nil
                if self.activeOperation === op { self.activeOperation = nil; self.busy = false }
                finish(result, true)
            }
        }
    }
    private func sessionStatus() -> [String: Any] {
        let portrait = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first?.interfaceOrientation == .portrait
        return ["transport": "online", "xctest_ready": ready && !needsRelaunch,
            "needs_passcode": needsPasscode, "needs_relaunch": needsRelaunch,
            "app_version": Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown",
            "orientation": portrait ? "portrait" : "other", "last_action_ok_at": lastActionOKAt as Any? ?? NSNull(),
            "runner_core": runnerCoreStatus()]
    }
    private func loadRunnerCore() -> AnyObject? {
        if let host = runnerCoreHost { return host }
        guard let url = Bundle.main.builtInPlugInsURL?.appendingPathComponent("RunnerCore.bundle"),
              let bundle = Bundle(url: url) else { log("runner core: RunnerCore.bundle missing"); return nil }
        do { try bundle.loadAndReturnError() } catch { log("runner core: load failed: \(error)"); return nil }
        guard let host: AnyObject = bundle.principalClass ?? NSClassFromString("IPURunnerCoreHost") else {
            log("runner core: IPURunnerCoreHost missing from the bundle"); return nil
        }
        runnerCoreHost = host
        return host
    }
    /// Main only, once the daemon session is ready. Needs the gateway pairing: the token is derived from its PSK.
    func startRunnerCore() {
        guard runnerCore, ready else { return }
        guard let pairing = remotePairing else { log("runner core: no gateway pairing to derive the token from; not started"); return }
        guard let host = loadRunnerCore() else { return }
        let arguments = ProcessInfo.processInfo.arguments
        if !arguments.contains("--runner-core-uiconfig-off"), let identifier = processSessionIdentifier {
            let ui = host.perform(NSSelectorFromString("installUITestingConfiguration:"), with: identifier)?.takeUnretainedValue()
            log("runner core: UI testing configuration \(ui.map { String(describing: $0) } ?? "nil")")
        }
        let options: NSDictionary = ["token": pairing.runnerToken]
        let result = host.perform(NSSelectorFromString("start:"), with: options)?.takeUnretainedValue()
        log("runner core: start \(result.map { String(describing: $0) } ?? "nil")")
    }
    func stopRunnerCore() {
        guard let host = runnerCoreHost else { return }
        _ = host.perform(NSSelectorFromString("stop"))
        log("runner core: stopped")
    }
    private func runnerCoreStatus() -> [String: Any] {
        var value = (runnerCoreHost?.perform(NSSelectorFromString("status"))?.takeUnretainedValue() as? [String: Any]) ?? ["running": false]
        value["enabled"] = runnerCore
        return value
    }
    private func configureAutoSession() {
        autoEpoch += 1
        autoTimer?.invalidate(); autoTimer = nil
        autoChecking = false
        autoNextProbe = .distantPast; autoNextCheck = .distantPast; autoWaitLogged = .distantPast
        guard autoSession else { return }
        autoTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.autoTick() }
        DispatchQueue.main.async { [weak self] in self?.autoTick() }
    }
    // Shared exact-address/lowest-index rule for manual Start and automatic VPN detection.
    private func vpnInterface(in entries: [[String: Any]], address: String) -> [String: Any]? {
        entries.filter { ($0["name"] as? String)?.hasPrefix("utun") == true && ($0["address"] as? String) == address }
            .sorted { ($0["index"] as? Int ?? 0) < ($1["index"] as? Int ?? 0) }.first
    }
    private func autoTick() {
        guard autoSession else { return }
        let now = Date()
        if state == "idle" {
            guard !autoMountFailed, !autoSessionAttempted, !needsPasscode, !needsRelaunch else { return }
            autoNextCheck = .distantPast
            guard now >= autoNextProbe, !autoProbeInFlight else { return }
            autoNextProbe = now.addingTimeInterval(5)
            guard !busy, !axInFlight else { log("auto session: VPN probe skipped (busy / AX in flight)"); return }
            autoProbeInFlight = true
            let epoch = autoEpoch
            worker.async {
                let json = ipu_boot_list_interfaces_json().map { String(cString: $0) }
                let entries = json.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [[String: Any]] }
                DispatchQueue.main.async {
                    self.autoProbeInFlight = false
                    guard self.autoSession, epoch == self.autoEpoch, self.state == "idle" else { return }
                    if let entries, self.vpnInterface(in: entries, address: "10.7.1.1") != nil {
                        guard !self.busy, !self.axInFlight else { self.log("auto session: Start skipped (busy / AX in flight)"); return }
                        guard !self.autoSessionAttempted else { return }
                        self.start()
                    } else if Date().timeIntervalSince(self.autoWaitLogged) >= 60 {
                        self.autoWaitLogged = Date()
                        self.log("auto session: waiting for utun 10.7.1.1")
                    }
                }
            }
            return
        }
        guard ready else { return }
        if !axLoadAttempted {
            guard !busy, !axInFlight else { return }
            inspectAX(load: true, variant: "E1c")
            autoNextCheck = now.addingTimeInterval(60)
            return
        }
        if autoNextCheck == .distantPast { autoNextCheck = now.addingTimeInterval(60) }
        guard now >= autoNextCheck else { return }
        autoNextCheck = now.addingTimeInterval(60)
        autoChecks += 1
        let at = ISO8601DateFormatter().string(from: now)
        guard !busy, !axInFlight, !autoChecking else {
            log("auto check \(autoChecks): skipped (busy / AX in flight / previous check running)")
            lastAutoCheck = ["at": at, "result": "skipped: busy / AX in flight / previous check running"]
            writeAutoStatus(); return
        }
        autoChecking = true
        let epoch = autoEpoch, token = generation
        let takeScreenshot = autoChecks % 5 == 0
        readScreen { result in
            guard self.autoSession, epoch == self.autoEpoch else { return }
            self.lastAutoCheck = ["at": at, "result": String(result.prefix(300))]
            self.writeAutoStatus()
            guard takeScreenshot, self.ready, token == self.generation else { self.autoChecking = false; return }
            guard !self.busy, !self.axInFlight else {
                self.log("auto screenshot: skipped (busy / AX in flight)")
                self.autoChecking = false; return
            }
            self.screenshot { result in
                guard self.autoSession, epoch == self.autoEpoch else { return }
                self.lastAutoScreenshot = ["at": ISO8601DateFormatter().string(from: Date()), "result": result]
                self.autoChecking = false
                self.writeAutoStatus()
            }
        }
    }
    private func writeAutoStatus() {
        let status: [String: Any] = ["version": 1, "updatedAt": ISO8601DateFormatter().string(from: Date()),
            "pid": Int(getpid()), "runID": mailbox?.runID.uuidString as Any? ?? NSNull(),
            "state": state, "ready": ready, "bootState": bootState, "bootDisconnected": bootDisconnected,
            "needs_passcode": needsPasscode, "needs_relaunch": needsRelaunch || SpikeBridge.needsRelaunch(),
            "axLoad": ["variant": axLoadVariant as Any? ?? NSNull(), "success": axLoadSuccess as Any? ?? NSNull()],
            "runnerCore": runnerCoreStatus(), "checks": autoChecks, "lastCheck": lastAutoCheck, "lastScreenshot": lastAutoScreenshot]
        do {
            let data = try JSONSerialization.data(withJSONObject: status, options: [.sortedKeys])
            try data.write(to: documents.appendingPathComponent("auto-status.json"), options: .atomic)
        } catch { log("auto status file error: \(error.localizedDescription)") }
    }
    private func pollMailbox() {
        mailbox?.poll(state: ["state": state, "ready": ready, "busy": busy,
            "axInFlight": axInFlight, "axLoadAttempted": axLoadAttempted, "hits": hits,
            "observeDisconnect": observeDisconnect, "bootDisconnected": bootDisconnected,
            "bootDisconnectedAt": bootDisconnectedAt, "bootState": bootState,
            "pid": Int(getpid()), "bundleID": Bundle.main.bundleIdentifier ?? "unknown",
            "foreground": UIApplication.shared.applicationState == .active]) { command in
            self.executeResearchCommand(command)
        }
    }
    private func executeResearchCommand(_ command: ResearchCommand) {
        if command.action == "remote-status" {
            mailbox?.finish(command.id, result: "remote mode=\(remoteMode) paired=\(remotePairing != nil) transport=\(remoteLink.status)")
            return
        }
        // Use exactly the UI's existing operations and safety gates; no second XCTest session.
        guard !busy, !axInFlight else {
            mailbox?.finish(command.id, result: "busy or AX still running", rejected: true); return
        }
        guard command.action == "start" ? (state == "idle") : ready else {
            mailbox?.finish(command.id, result: "session not ready for command", rejected: true); return
        }
        guard !["e1", "e1b", "e1c"].contains(command.action) || !axLoadAttempted else {
            mailbox?.finish(command.id, result: "AX load (E1/E1b/E1c) already attempted in this process", rejected: true); return
        }
        guard command.action != "tap" || (target != nil && UIApplication.shared.applicationState == .active) else {
            mailbox?.finish(command.id, result: "target unavailable or app not foreground", rejected: true); return
        }
        log("research command \(command.id.uuidString): \(command.action)")
        dispatchingCommandID = command.id
        defer { dispatchingCommandID = nil }
        switch command.action {
        case "start": startCommandID = command.id; start()
        case "ddi-probe": ddiProbe(command.id)
        case "ddi-ticket": ddiTicket(command.id)
        case "ddi-mount": ddiMount(command.id)
        case "tap-offer": tapOffer()
        case "e0": inspectAX(load: false)
        case "e1": inspectAX(load: true)
        case "e1b": inspectAX(load: true, variant: "E1b")
        case "e1c": inspectAX(load: true, variant: "E1c")
        case "read": readScreen()
        case "read-runner": readScreen(runnerParameters: true)
        case "read-delayed": readScreen(delayed: true)
        case "screenshot": screenshot()
        case "tap": tap()
        default: mailbox?.finish(command.id, result: "unsupported command", rejected: true)
        }
    }
    // Private signatures stay outside File Sharing Documents and are excluded from backup.
    private func ddiDirectories() throws -> (ddi: URL, store: URL) {
        guard let ddi = Bundle.main.url(forResource: "DDI", withExtension: nil) else {
            throw NSError(domain: "MissingDDIResources", code: 1)
        }
        var store = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            .appendingPathComponent("ddi-ticket", isDirectory: true)
        try FileManager.default.createDirectory(at: store, withIntermediateDirectories: true,
            attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], ofItemAtPath: store.path)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try store.setResourceValues(values)
        return (ddi, store)
    }
    // Worker only: copy the borrowed JSON before another ABI call; never expose last_error.
    private func ddiResult(handle: OpaquePointer, mount: Bool) -> String {
        do {
            let paths = try ddiDirectories()
            return paths.ddi.path.withCString { ddiPath in
                paths.store.path.withCString { storePath in
                    let ptr = mount ? ipu_boot_ddi_mount_json(handle, ddiPath, storePath)
                        : ipu_boot_ddi_ticket_json(handle, ddiPath, storePath)
                    guard let ptr else { return "{\"status\":\"error\",\"error\":\"DdiAbiFailure\"}" }
                    return String(cString: ptr)
                }
            }
        } catch {
            let reason = (error as NSError).domain == "MissingDDIResources" ? "MissingDDIResources" : "TicketStoreSetup"
            return "{\"status\":\"error\",\"error\":\"\(reason)\"}"
        }
    }
    private func saveDdiMountReport(_ result: String) {
        let timestamp = ISO8601DateFormatter().string(from: Date())
        do {
            try Data(result.utf8).write(to: documents.appendingPathComponent("ddi-mount-\(timestamp).json"), options: .atomic)
        } catch { log("ddi-mount: ReportStoreFailure") }
    }
    private func ddiTicket(_ commandID: UUID) { ddiCommand(commandID, mount: false) }
    private func ddiMount(_ commandID: UUID) { ddiCommand(commandID, mount: true) }
    private func ddiCommand(_ commandID: UUID, mount: Bool) {
        guard let op = beginOperation(mount ? "ddi-mount" : "ddi-ticket") else { return }
        let token = generation
        worker.async {
            var result = "{\"status\":\"error\",\"error\":\"NoStartedHandle\"}"
            if self.workerToken == token, let handle = self.handle {
                result = self.ddiResult(handle: handle, mount: mount)
            }
            if mount { self.saveDdiMountReport(result) }
            let completed = result
            DispatchQueue.main.async {
                self.log("\(mount ? "ddi-mount" : "ddi-ticket"): \(completed)")
                self.finishOperation(op, result: completed)
            }
        }
    }
    private func ddiProbe(_ commandID: UUID) {
        guard let op = beginOperation("ddi-probe") else { return }
        let token = generation
        // Keep the operation gate held for the entire multi-request probe;
        // the watchdog reports still_running without releasing it.
        worker.async {
            let result: String
            if self.workerToken == token, let handle = self.handle {
                if let ptr = ipu_boot_ddi_probe_json(handle) {
                    result = String(cString: ptr) // copy thread-local borrowed JSON here
                } else {
                    result = "{\"error\":\"ProbeAbiFailure\"}"
                }
            } else {
                result = "{\"error\":\"NoStartedHandle\"}"
            }
            DispatchQueue.main.async {
                self.log("ddi-probe: \(result)")
                self.finishOperation(op, result: result)
            }
        }
    }
    func log(_ message: String) {
        if !Thread.isMainThread { DispatchQueue.main.async { self.log(message) }; return }
        let line = "\(ISO8601DateFormatter().string(from: Date())) \(message)\n"
        logs += line
        if logs.count > 100_000 { logs = String(logs.suffix(80_000)) }
        let url = documents.appendingPathComponent("selfrunner.log")
        do {
            if !FileManager.default.fileExists(atPath: url.path) { try Data().write(to: url) }
            let file = try FileHandle(forWritingTo: url)
            defer { try? file.close() }
            try file.seekToEnd()
            try file.write(contentsOf: Data(line.utf8))
        } catch { logs += "log file error: \(error)\n" }
        NSLog("%@", message)
    }
    func updateLocation() {
        guard keepLocation else { location.stopUpdatingLocation(); log("background location stopped"); return }
        switch location.authorizationStatus {
        case .notDetermined: location.requestWhenInUseAuthorization()
        case .authorizedWhenInUse:
            location.requestAlwaysAuthorization()
            location.allowsBackgroundLocationUpdates = true
            location.startUpdatingLocation()
            log("location WhenInUse; choose Always in Settings for background survival")
        case .authorizedAlways:
            location.allowsBackgroundLocationUpdates = true
            location.startUpdatingLocation()
            log("background location enabled (Always); no coordinates stored")
        default: log("location permission denied/restricted; background survival not assured")
        }
    }
    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        if state != "idle" || remoteMode { updateLocation() }
    }
    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) { remoteLink.reconnectNow() }
    func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) { log("location: \(error)") }
    private func cError() -> String { String(cString: ipu_boot_last_error()) }
    private func cReply() -> String { String(cString: ipu_boot_last_json()) }
    func start() {
        guard state == "idle", !busy, !automationInFlight else { return }
        needsRelaunch = needsRelaunch || SpikeBridge.needsRelaunch()
        guard !needsRelaunch else { log("needs_relaunch: terminate and relaunch the process before Start"); return }
        needsPasscode = false
        autoMountFailed = false
        busy = true; stopping = false; generation += 1
        let token = generation
        let local = interfaceIP.trimmingCharacters(in: .whitespacesAndNewlines)
        let peer = peerIP.trimmingCharacters(in: .whitespacesAndNewlines)
        let manual = manualPort.trimmingCharacters(in: .whitespacesAndNewlines)
        let automatic = automaticPort
        let authorize = authorizePID
        if processSessionIdentifier == nil { processSessionIdentifier = UUID().uuidString.uppercased() }
        UserDefaults.standard.set(local, forKey: "rpInterfaceIP")
        UserDefaults.standard.set(peer, forKey: "rpPeerIP")
        UserDefaults.standard.set(manual, forKey: "rpManualPort")
        UserDefaults.standard.set(automatic, forKey: "rpAutomaticPort")
        state = "finding VPN interface"
        updateLocation()
        // Copy thread-local ABI JSON on the serial worker before returning to main.
        worker.async {
            guard let ptr = ipu_boot_list_interfaces_json() else { self.fail(self.cError(), token: token); return }
            let json = String(cString: ptr)
            self.log("interfaces: \(json)")
            DispatchQueue.main.async {
                guard token == self.generation else { return }
                self.prepareConnection(json: json, local: local, peer: peer, automatic: automatic, manual: manual, authorize: authorize, token: token)
            }
        }
    }
    private func prepareConnection(json: String, local: String, peer: String, automatic: Bool, manual: String, authorize: Bool, token: Int) {
        func validIPv4(_ text: String) -> Bool {
            var address = in_addr()
            return text.withCString { inet_pton(AF_INET, $0, &address) } == 1
        }
        guard validIPv4(local), validIPv4(peer) else { fail("接口地址和对端地址必须是 IPv4", token: token); return }
        guard let entries = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [[String: Any]] else {
            fail("cannot parse interface JSON", token: token); return
        }
        let utuns = entries.filter { ($0["name"] as? String)?.hasPrefix("utun") == true }
        guard let match = vpnInterface(in: entries, address: local), let iface = match["name"] as? String else {
            fail("未找到地址 \(local) 的 utun；请连接 LocalDevVPN 并检查接口地址。所有 utun: \(utuns)", token: token); return
        }
        log("VPN interface selected: \(iface), index=\(match["index"] ?? "?"), address=\(local); exact address match, lowest index on ties")
        if !automatic {
            guard let port = UInt16(manual), port > 0 else { fail("手动端口必须为 1–65535", token: token); return }
            log("RP manual port: \(port)")
            connect(endpoint: "\(peer):\(port)", iface: iface, local: local, authorize: authorize, token: token)
            return
        }
        state = "Bonjour discovery (6 s)"
        let addresses = entries.compactMap { $0["address"] as? String }
        let host = RPDiscovery.localHost
        log("Bonjour selection rule: local hostname \(host) or any local interface address \(addresses); latest resolved local candidate wins")
        discovery.start(log: { self.log($0) }) { candidates in
            guard token == self.generation else { return }
            for candidate in candidates {
                self.log("Bonjour candidate \(candidate.service), host=\(candidate.host), addresses=\(candidate.addresses), port=\(candidate.port): \(RPDiscovery.localReason(candidate, addresses: addresses, host: host) ?? "not a valid local service")")
            }
            let selected = RPDiscovery.select(candidates, addresses: addresses, host: host)
            let port = selected?.port ?? 49152
            if let selected {
                let reason = RPDiscovery.localReason(selected, addresses: addresses, host: host) ?? "?"
                self.log("Bonjour selected \(selected.service), port=\(port): \(reason); latest resolved local candidate; connect peer \(peer):\(port)")
            } else {
                self.log("Bonjour: no resolved local service with valid port; fallback to 49152 (resolved candidates=\(candidates.count))")
            }
            self.connect(endpoint: "\(peer):\(port)", iface: iface, local: local, authorize: authorize, token: token)
        }
    }
    private func connect(endpoint: String, iface: String, local: String, authorize: Bool, token: Int, didMountReconnect: Bool = false) {
        guard token == self.generation, let sessionIdentifier = processSessionIdentifier else { return }
        log("RP connect \(endpoint) via \(iface)/\(local), deadline 35 s")
        self.state = "connecting RP"
        let shouldAutoMount = autoMount
        self.worker.async {
            self.workerToken = token
            let file = self.documents.appendingPathComponent("rp_pairing.plist").path
            let start = Date()
            self.handle = file.withCString { path in endpoint.withCString { addr in
                iface.withCString { name in local.withCString { ip in
                    ipu_boot_connect_rp_bound(path, addr, name, ip, 35)
                } }
            } }
            guard let handle = self.handle else { self.fail(self.cError(), token: token); return }
            self.log("RP connected in \(Int(Date().timeIntervalSince(start)*1000)) ms")
            var mounted: Int32 = 0; var developer: Int32 = 0
            guard ipu_boot_ddi_status(handle, &mounted, &developer) == 0 else { self.fail(self.cError(), token: token); return }
            self.log("DDI mounted=\(mounted), developer mode=\(developer)")
            if mounted == 0, developer == 1, shouldAutoMount, !didMountReconnect {
                DispatchQueue.main.async {
                    guard token == self.generation else { return }
                    self.autoMountFailed = true // Suppress automatic retries until the rebuilt session is ready.
                    self.state = "mounting DDI: preparing"
                }
                let result = self.ddiResult(handle: handle, mount: true)
                self.log("ddi-mount: \(result)")
                self.saveDdiMountReport(result)
                let report = try? JSONSerialization.jsonObject(with: Data(result.utf8)) as? [String: Any]
                guard report?["status"] as? String == "mounted" else {
                    self.fail("DDI mount stopped: \(result)", token: token, mountFailure: true); return
                }
                ipu_boot_free(handle)
                self.handle = nil
                DispatchQueue.main.async {
                    guard token == self.generation else { return }
                    self.state = "mounting DDI: refreshing RSD"
                    self.connect(endpoint: endpoint, iface: iface, local: local, authorize: authorize,
                        token: token, didMountReconnect: true)
                }
                return
            }
            guard mounted == 1, developer == 1 else { self.fail("Mount personalized DDI with Xcode and enable Developer Mode first", token: token, mountFailure: didMountReconnect); return }
            // Captured on main when Start begins; stable across Stop/Start in this process.
            let uuid = sessionIdentifier
            guard uuid.withCString({ ipu_boot_start_test_sessions(handle, $0) }) == 0 else { self.fail(self.cError(), token: token); return }
            self.log("IDE sessions uuid=\(uuid), replies=\(self.cReply())")
            // Names are confirmed in Xcode's XCTestCore; no test bundle/configuration is loaded.
            setenv("XCTestSessionIdentifier", uuid, 1)
            setenv("XCTestManagerVariant", "DDI", 1)
            setenv("XCTestConfigurationFilePath", "", 1)
            DispatchQueue.main.async {
                guard token == self.generation else { return }
                self.state = "waiting for daemon session (90 s)"
                self.beginStatusPolling(token: token)
                self.autoSessionAttempted = true // Includes a manual first initialization.
                SpikeBridge.loadXCTest(log: { self.log($0) }) { error in
                    guard token == self.generation else { return }
                    if let error {
                        if error.contains("needs_relaunch:") { self.needsRelaunch = true }
                        self.fail(error, token: token); return
                    }
                    self.worker.async {
                        guard self.workerToken == token, self.handle != nil else { return }
                        if authorize {
                            let result = ipu_boot_authorize_pid(handle, UInt32(getpid()))
                            self.log("authorize own pid: rc=\(result), reply=\(result == 0 ? self.cReply() : self.cError())")
                            if result != 0 { self.fail(self.cError(), token: token); return }
                        }
                        DispatchQueue.main.async {
                            guard token == self.generation else { return }
                            self.prepareAutomation(token: token)
                        }
                    }
                }
            }
        }
    }
    private func prepareAutomation(token: Int) {
        automationInFlight = true; automationTimedOut = false
        state = "checking/enabling automation mode (20 s)"
        // A separate queue keeps boot status/Stop responsive even if private enable hangs.
        DispatchQueue.main.asyncAfter(deadline: .now() + 20) {
            guard token == self.generation, self.automationInFlight else { return }
            self.automationTimedOut = true; self.needsPasscode = true; self.ready = false
            self.state = "automation mode timeout; enter phone passcode if prompted"
            self.log("ERROR: Timed out while enabling automation mode (20 s); needs_passcode=true; private call may still be running")
            self.mailbox?.finish(self.startCommandID, result: "automation timeout; needs_passcode=true")
            self.startCommandID = nil
            // Do not release the gate or retry until the underlying call returns.
        }
        automationWorker.async {
            let enabled = SpikeBridge.automationModeEnabled(log: { self.log($0) })?.boolValue == true
            var error: String?
            if enabled { self.log("automation mode already enabled") }
            else {
                self.log("enableAutomationModeWithError:; enter phone passcode if prompted; timeout 20 s")
                error = SpikeBridge.enableAutomation()
            }
            DispatchQueue.main.async {
                self.automationInFlight = false
                guard token == self.generation else { return }
                if let error {
                    if self.automationTimedOut || error.localizedCaseInsensitiveContains("timed out") || error.localizedCaseInsensitiveContains("timeout") {
                        self.needsPasscode = true
                    }
                    self.fail(error, token: token); return
                }
                self.needsPasscode = false
                self.needsRelaunch = self.needsRelaunch || SpikeBridge.needsRelaunch()
                guard !self.needsRelaunch else { self.fail("needs_relaunch: shared daemon session invalidated during automation setup", token: token); return }
                self.log("quiescence bypass installed: \(SpikeBridge.installQuiescenceBypass())")
                self.autoMountFailed = false
                self.ready = true; self.busy = false; self.state = "ready"
                self.startRunnerCore()
                self.log("ready: automation enabled; no test plan or explicit UI testing initialization. E0/E1 are manual experiments.")
                self.mailbox?.finish(self.startCommandID, result: "ready")
                self.startCommandID = nil
            }
        }
    }
    private func beginStatusPolling(token: Int) {
        // A session established in observation mode stays observational when auto is switched off.
        var observe = observeDisconnect
        timer?.invalidate()
        timer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { _ in
            self.worker.async {
                guard self.workerToken == token, let handle = self.handle, let ptr = ipu_boot_status_json(handle) else { return }
                let json = String(cString: ptr)
                self.log("boot status: \(json)")
                let parsed = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]
                let bootState = String((parsed?["state"].map { String(describing: $0) } ?? json).prefix(300))
                DispatchQueue.main.async {
                    guard token == self.generation else { return }
                    self.bootState = bootState
                    if self.autoSession { observe = true }
                    if observe, !self.bootDisconnected,
                       json.contains("\"Disconnected\"") || json.contains("\"Closed\"") {
                        self.bootDisconnected = true
                        self.bootDisconnectedAt = Date().timeIntervalSince1970
                        self.log("boot disconnected (observe mode; XCTest kept): \(json)")
                    }
                    if !observe && !self.autoSession && (json.contains("\"Disconnected\"") || json.contains("\"Closed\"")) {
                        self.fail("boot connection disconnected", token: token)
                    }
                }
            }
        }
    }
    private func fail(_ error: String, token: Int, mountFailure: Bool = false) {
        DispatchQueue.main.async {
            guard token == self.generation else { return }
            if mountFailure { self.autoMountFailed = true }
            if self.autoSession && !mountFailure { self.autoRetryAfterFailure = true }
            self.log("ERROR: \(error)")
            self.mailbox?.finish(self.startCommandID, result: "ERROR: \(error)")
            self.startCommandID = nil
            self.stop()
        }
    }
    func stop() {
        guard !stopping, activeOperation == nil else {
            log("stop refused: still_running"); return
        }
        mailbox?.finish(startCommandID, result: "stopped before ready", rejected: true)
        startCommandID = nil
        activeOperation = nil
        autoEpoch += 1 // Discard automatic continuations from the previous session.
        stopping = true; generation += 1; ready = false; busy = true; state = "stopping"
        bootDisconnected = false; bootDisconnectedAt = 0; bootState = "unknown"
        timer?.invalidate(); timer = nil; discovery.cancel(); if !remoteMode { location.stopUpdatingLocation() }
        stopRunnerCore()
        SpikeBridge.reset()
        if axInFlight { log("AX call still running; Stop cannot cancel it. AX remains disabled until return; relaunch for a fresh experiment.") }
        worker.async {
            self.workerToken = 0
            if let handle = self.handle {
                ipu_boot_free(handle)
                let error = self.cError()
                if !error.isEmpty { self.log("disconnect: \(error)") }
            }
            self.handle = nil
            DispatchQueue.main.async {
                self.busy = false; self.stopping = false; self.state = "idle"
                self.autoChecking = false
                self.autoNextCheck = .distantPast
                if self.autoRetryAfterFailure { self.autoNextProbe = Date().addingTimeInterval(30) }
                self.autoRetryAfterFailure = false
                self.log("stopped: DTX/tunnel released; shared XCTest session retained for reuse; automation mode left enabled")
            }
        }
    }
    private func beginOperation(_ name: String, delay: TimeInterval = 0, requiresReady: Bool = true) -> Operation? {
        guard (!requiresReady || ready), !busy, activeOperation == nil else { return nil }
        busy = true
        let op = Operation(name, generation: generation, commandID: dispatchingCommandID)
        activeOperation = op
        DispatchQueue.main.asyncAfter(deadline: .now() + delay + 20) {
            guard !op.completed else { return }
            op.timedOut = true
            self.mailbox?.watchdog(op.commandID)
            if op.name.hasPrefix("remote ") {
                // Remote watchdog logging is installed per request with its stable ID.
            } else {
                self.log("\(op.name) still running after 20 s at step \(op.step)")
            }
        }
        return op
    }
    private func operationLog(_ op: Operation) -> (String) -> Void {
        { message in
            DispatchQueue.main.async {
                op.step = message
                self.log("\(op.name)\(op.timedOut || op.generation != self.generation ? " late" : ""): \(message)")
            }
        }
    }
    private func finishOperation(_ op: Operation, result: String) {
        op.completed = true
        let late = op.timedOut || op.generation != generation
        log("\(op.name)\(late ? " late" : "") \(Int(Date().timeIntervalSince(op.started)*1000)) ms: \(result)")
        mailbox?.finish(op.commandID, result: "\(op.name)\(late ? " late" : ""): \(result)")
        if activeOperation === op {
            activeOperation = nil
            busy = false
        }
    }
    // Research-only cross-app tap, limited to the App Store free get/redownload/update offer button.
    func tapOffer() {
        readScreen(holdGate: true) { result in
            guard let op = self.activeOperation else { return }
            let fail = { (why: String) in self.finishOperation(op, result: "tap-offer refused: \(why)") }
            guard result.contains("App Store"),
                  let fileRange = result.range(of: #"snapshots/[^,\]\s]+\.jsonl"#, options: .regularExpression),
                  let pidRange = result.range(of: #"\"pid\"\)?: (\d+)"#, options: .regularExpression) else { return fail("App Store not foreground or no snapshot") }
            let pid = Int32(result[pidRange].filter(\.isNumber)) ?? 0
            let url = self.documents.appendingPathComponent(String(result[fileRange]))
            guard pid > 0, let text = try? String(contentsOf: url, encoding: .utf8) else { return fail("snapshot unreadable") }
            let allowed = ["AppStore.offerButton[state=get]", "AppStore.offerButton[state=redownload]", "AppStore.offerButton[state=update]"]
            let rows = text.split(separator: "\n").compactMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: Any] }
            let offers = rows.filter { allowed.contains(($0["identifier"] as? String) ?? "") }
            guard offers.count == 1, let frame = offers[0]["frame"] as? String else { return fail("expected exactly one free offer button, found \(offers.count)") }
            let rect = NSCoder.cgRect(for: frame)
            guard rect.width > 0, rect.height > 0, UIScreen.main.bounds.contains(CGPoint(x: rect.midX, y: rect.midY)) else { return fail("offer frame off screen \(frame)") }
            let id = offers[0]["identifier"] as? String ?? ""
            DispatchQueue.global().async {
                let error = SpikeBridge.tapScreen(point: CGPoint(x: rect.midX, y: rect.midY), pid: pid, log: { self.log($0) })
                DispatchQueue.main.async {
                    let r = "tap-offer \(id) at \(frame): \(error ?? "SUCCESS")"
                    self.finishOperation(op, result: r)
                }
            }
        }
    }
    func readScreen(delayed: Bool = false, runnerParameters: Bool = false, holdGate: Bool = false, completion: ((String) -> Void)? = nil) {
        guard !axInFlight else {
            log("AX call still running; read screen refused")
            completion?("refused: AX in flight")
            return
        }
        let delay: TimeInterval = delayed ? 5 : 0
        let depth = maxDepth
        let name = runnerParameters ? "read screen (runner parameters)" : (delayed ? "delayed read screen" : "read screen")
        guard let op = beginOperation(name, delay: delay) else {
            completion?("refused: operation gate")
            return
        }
        axInFlight = true
        if delayed { log("请在 5 秒内切到其它 App（例如设置）；只读，不点击。") }
        // Main schedules the delay; all AX work runs off main, including while backgrounded.
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            guard op.generation == self.generation else {
                self.axInFlight = false
                self.finishOperation(op, result: "cancelled before dispatch (session changed)")
                completion?("cancelled before dispatch (session changed)")
                return
            }
            let logger = self.operationLog(op)
            self.axWorker.async {
                let result = SpikeBridge.readScreen(maxDepth: depth, runnerParameters: runnerParameters, log: logger)
                DispatchQueue.main.async {
                    self.axInFlight = false
                    if !holdGate { self.finishOperation(op, result: "\(result)") }
                    completion?("\(result)")
                }
            }
        }
    }
    func inspectAX(load: Bool, variant: String = "E1") {
        guard !axInFlight, !load || !axLoadAttempted,
              let op = beginOperation(load ? "\(variant) load Accessibility" : "E0 UI testing state") else { return }
        axInFlight = true
        if load { axLoadAttempted = true; axLoadVariant = variant; axLoadSuccess = nil }
        let logger = operationLog(op)
        axWorker.async {
            let result: [AnyHashable: Any]
            if !load {
                result = SpikeBridge.inspectUIAutomation(log: logger)
            } else if variant == "E1" {
                result = SpikeBridge.loadAccessibility(log: logger)
            } else {
                result = SpikeBridge.loadAccessibilityVariant(variant, log: logger)
            }
            DispatchQueue.main.async {
                self.axInFlight = false
                if load { self.axLoadSuccess = (result["success"] as? NSNumber)?.boolValue ?? false }
                self.finishOperation(op, result: "\(result)")
            }
        }
    }
    // Called only on screenshotWorker, so writes and retention are serialized.
    private func saveScreenshot(_ data: Data?, kind: String, error: String? = nil) -> String {
        guard let data else { return " file-error=\(error ?? "screenshot returned no data")" }
        let directory = documents.appendingPathComponent("screenshots", isDirectory: true)
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "yyyyMMdd'T'HHmmss.SSS'Z'"
        let filename = "\(formatter.string(from: Date()))-\(kind)-\(getpid()).png"
        let manager = FileManager.default
        do {
            try manager.createDirectory(at: directory, withIntermediateDirectories: true)
            try data.write(to: directory.appendingPathComponent(filename), options: .atomic)
        } catch {
            return " file-error=\(error.localizedDescription)"
        }
        do {
            let files = try manager.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.isRegularFileKey])
                .filter {
                    guard $0.pathExtension == "png" else { return false }
                    return try $0.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true
                }
                .sorted { $0.lastPathComponent < $1.lastPathComponent }
            for file in files.prefix(max(0, files.count - 30)) { try manager.removeItem(at: file) }
        } catch {
            log("screenshot retention error: \(error.localizedDescription)")
        }
        return " file=screenshots/\(filename)"
    }
    func screenshot(completion: ((String) -> Void)? = nil) {
        guard let op = beginOperation("screenshot") else {
            completion?("refused: operation gate")
            return
        }
        let logger = operationLog(op)
        screenshotWorker.async {
            var error: NSString?
            let data = SpikeBridge.screenshot(log: logger, error: &error)
            let fileResult = self.saveScreenshot(data, kind: "shot", error: error as String?)
            DispatchQueue.main.async {
                if op.generation == self.generation { self.image = data.flatMap { UIImage(data: $0) } }
                let result = "bytes=\(data?.count ?? 0), \(error as String? ?? "OK")\(fileResult)"
                self.finishOperation(op, result: result)
                completion?(result)
            }
        }
    }
    func tap() {
        guard let target, let op = beginOperation("target tap") else { return }
        let before = hits
        SpikeBridge.tapTarget(target, worker: tapWorker, log: operationLog(op)) { error in
            // Allow the target action queued on main to settle before checking its counter.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
                let success = error == nil && self.hits == before + 1
                let result = "counter \(before) -> \(self.hits): \(error ?? (self.hits == before + 1 ? "SUCCESS" : "FAIL: target counter did not increment exactly once"))"
                if success {
                    self.finishOperation(op, result: result)
                    return
                }
                let logger = self.operationLog(op)
                self.screenshotWorker.async {
                    var screenshotError: NSString?
                    let data = SpikeBridge.screenshot(log: logger, error: &screenshotError)
                    let fileResult = self.saveScreenshot(data, kind: "tapfail", error: screenshotError as String?)
                    DispatchQueue.main.async {
                        self.finishOperation(op, result: result + fileResult)
                    }
                }
            }
        }
    }

}

struct TargetButton: UIViewRepresentable {
    @ObservedObject var model: RunnerModel
    func makeUIView(context: Context) -> UIButton {
        let button = UIButton(type: .system)
        button.accessibilityIdentifier = "selfrunner-click-target"
        button.addAction(UIAction { [weak model] _ in model?.hits += 1 }, for: .touchUpInside)
        model.target = button
        return button
    }
    func updateUIView(_ button: UIButton, context: Context) {
        button.setTitle("点击靶 · \(model.hits)", for: .normal)
        button.isEnabled = model.ready // Remains enabled during synthesis so it can receive the touch.
    }
}
struct ContentView: View {
    @ObservedObject var model: RunnerModel
    var body: some View {
        VStack(spacing: 8) {
            Text("Self Runner · \(model.state)").font(.headline)
            Group {
                TextField("接口地址（10.7.1.1）", text: $model.interfaceIP)
                TextField("对端地址（10.7.0.1）", text: $model.peerIP)
                Picker("端口", selection: $model.automaticPort) {
                    Text("自动（Bonjour）").tag(true)
                    Text("手动").tag(false)
                }.pickerStyle(.segmented)
                TextField("手动端口", text: $model.manualPort).keyboardType(.numberPad).disabled(model.automaticPort)
            }.textInputAutocapitalization(.never).autocorrectionDisabled()
                .textFieldStyle(.roundedBorder).disabled(model.state != "idle")
            Toggle("授权自身 PID（实验，默认关）", isOn: $model.authorizePID).disabled(model.state != "idle")
            Toggle("远程模式", isOn: $model.remoteMode)
            Button("重新配对") { model.rePair() }.disabled(model.busy)
            Toggle("自动会话（开机即连）", isOn: $model.autoSession)
            Toggle("自动挂载 DDI（实验）", isOn: $model.autoMount)
            Toggle("Runner API 8100/9100（实验）", isOn: $model.runnerCore)
            Toggle("后台定位保活（不记录位置）", isOn: $model.keepLocation)
                .onChange(of: model.keepLocation) { _, _ in
                    if model.state != "idle" && model.state != "stopping" { model.updateLocation() }
                }
            Text("先开启 LocalDevVPN。Start 后允许本地网络、定位，并输入手机密码。后台需定位 Always 权限。")
                .font(.caption)
            HStack {
                Button("Start") { model.start() }.disabled(model.state != "idle" || model.busy)
                Button("停止") { model.stop() }.disabled(model.busy || model.state == "idle" || model.state == "stopping")
                Button("读屏") { model.readScreen() }.disabled(!model.ready || model.busy || model.axInFlight)
                Button("截图") { model.screenshot() }.disabled(!model.ready || model.busy)
            }
            Stepper("读屏 maxDepth：\(model.maxDepth)", value: $model.maxDepth, in: 1...100).disabled(model.busy)
            HStack {
                Button("E0 状态") { model.inspectAX(load: false) }.disabled(!model.ready || model.busy || model.axInFlight)
                Button("E1 加载 AX（一次）") { model.inspectAX(load: true) }.disabled(!model.ready || model.busy || model.axInFlight || model.axLoadAttempted)
            }
            Button("E1c 加载 AX") { model.inspectAX(load: true, variant: "E1c") }.disabled(!model.ready || model.busy || model.axInFlight || model.axLoadAttempted)
            if model.axInFlight { Text("AX 调用仍在执行；超时提示不会取消调用。").font(.caption) }
            Button("读屏（runner 参数）") { model.readScreen(runnerParameters: true) }.disabled(!model.ready || model.busy || model.axInFlight)
            Button("5 秒后读屏") { model.readScreen(delayed: true) }.disabled(!model.ready || model.busy || model.axInFlight)
            TargetButton(model: model).frame(height: 48).background(Color.blue.opacity(0.15)).cornerRadius(8)
            Button("点自测按钮") { model.tap() }.disabled(!model.ready || model.busy)
            if let image = model.image { Image(uiImage: image).resizable().scaledToFit().frame(maxHeight: 130) }
            ScrollView { Text(model.logs).font(.system(size: 10, design: .monospaced))
                .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled) }
        }.padding()
    }
}
