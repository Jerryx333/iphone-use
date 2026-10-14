import Foundation
import UIKit
import ImageIO
import CoreFoundation

struct RemoteActionResult {
    var result: [String: Any]?
    var code: String
    init(_ result: [String: Any]) { self.result = result; code = "ok" }
    init(code: String, details: [String: Any]? = nil) { result = details; self.code = code }
}

// Accessed only by the action worker, under RunnerModel's shared execution gate.
final class RemoteActions {
    private var snapshots: [[String: Any]] = []
    // The Objective-C bridge can encode this flag as either NSNumber or Bool.
    // Missing or invalid flags cannot establish that a snapshot is complete.
    private func isTruncated(_ value: Any?) -> Bool {
        guard let value = value as? NSNumber else { return true }
        if value == NSNumber(value: 0) { return false }
        if value == NSNumber(value: 1) { return true }
        return true
    }
    private func read(_ depth: Int) -> RemoteActionResult {
        let snapshot = SpikeBridge.remoteRead(maxDepth: depth) as? [String: Any] ?? [:]
        guard snapshot["error"] == nil, snapshot["nodes"] != nil else { return RemoteActionResult(code: "read_failed") }
        snapshots.append(snapshot)
        if snapshots.count > 4 { snapshots.removeFirst() }
        return RemoteActionResult(snapshot)
    }
    private func readForTap(_ depth: Int, expired: () -> Bool) -> RemoteActionResult {
        var fresh = read(depth)
        if expired() { return RemoteActionResult(code: "expired") }
        for nextDepth in [24, 60, 100] where nextDepth > depth {
            guard fresh.code == "ok", isTruncated(fresh.result?["truncated"]) else { break }
            if expired() { return RemoteActionResult(code: "expired") }
            fresh = read(nextDepth)
            if expired() { return RemoteActionResult(code: "expired") }
        }
        return fresh
    }
    private func lock(strategy: String, expired: () -> Bool) -> RemoteActionResult {
        let strategies = [strategy] // Experimental XCTest strategies are explicit only.
        var attempts: [[String: Any]] = []
        var used = strategy
        var sentSuccessfully = false
        func result(_ locked: Bool, code: String = "ok") -> RemoteActionResult {
            let details: [String: Any] = ["locked": locked ? true : "unconfirmed",
                                          "strategy_used": used, "attempts": attempts]
            return code == "ok" ? RemoteActionResult(details) : RemoteActionResult(code: code, details: details)
        }
        let lockLabels: Set<String> = ["已锁定", "向上轻扫以解锁", "Locked", "Swipe up to unlock"]
        for candidate in strategies {
            let entries: [String]
            switch candidate {
            case "hid": entries = ["_dispatchEventWithPage:usage:duration:", "performDeviceEvent:error:", "_XCT_performDeviceEvent:completion:"]
            case "device_event": entries = ["performDeviceEvent:error:"]
            default: entries = ["pressLockButton"]
            }
            for entry in entries {
                if expired() { return result(false, code: "expired") }
                used = candidate
                let attempt = SpikeBridge.remoteLock(entry: entry, strategy: candidate) as? [String: Any] ??
                    ["strategy": candidate, "entry": entry, "ms": 0, "error_code": "press_failed"]
                attempts.append(attempt)
                if expired() { return result(false, code: "expired") }
                guard attempt["error_code"] == nil else { continue }
                sentSuccessfully = true
                let end = ProcessInfo.processInfo.systemUptime + 2
                while ProcessInfo.processInfo.systemUptime < end {
                    if expired() { return result(false, code: "expired") }
                    // Preserve the existing label / SpringBoard foreground PID heuristic.
                    let snapshot = SpikeBridge.remoteRead(maxDepth: 100) as? [String: Any] ?? [:]
                    let marked = snapshot["error"] == nil && !unlockLabels(snapshot).isDisjoint(with: lockLabels)
                    let springBoardPID = SpikeBridge.remoteSystemPID()
                    let pid = foregroundPID()
                    if expired() { return result(false, code: "expired") }
                    // A blocking AX read can finish after the confirmation window.
                    if ProcessInfo.processInfo.systemUptime <= end,
                       marked || (springBoardPID > 0 && pid == springBoardPID) { return result(true) }
                    if !pause(0.3, until: end, expired: expired) { return result(false, code: "expired") }
                }
            }
        }
        return result(false, code: sentSuccessfully ? "ok" : (attempts.last?["error_code"] as? String ?? "unsupported"))
    }
    // All navigation is deadline-bound; the editor's Play tap is sent at most once.
    private func shortcutLock(name: String, portrait: Bool, expired: () -> Bool) -> RemoteActionResult {
        var attempts: [[String: Any]] = []
        var path = "url"
        var actionSent = false
        var stage: String?
        var lastRead: [String: Any]?
        // Keep only read metadata; AX error descriptions can contain private text.
        func pollRead() -> (snapshot: [String: Any], foregroundPID: Int32, ok: Bool) {
            let snapshot = SpikeBridge.remoteRead(maxDepth: 100) as? [String: Any] ?? [:]
            let foreground = SpikeBridge.remoteForeground() as? [String: Any] ?? [:]
            let pid = (foreground["pid"] as? NSNumber)?.int32Value ?? 0
            let readNodes = snapshot["nodes"] as? [[String: Any]]
            let ok = snapshot["error"] == nil && readNodes != nil
            lastRead = ["ok": ok, "error_code": ok ? "ok" : "read_failed",
                "node_count": readNodes?.count ?? 0, "foreground_pid": pid,
                "pid": snapshot["pid"] ?? NSNull(),
                "truncated": snapshot["truncated"] ?? NSNull()]
            return (snapshot, pid, ok)
        }
        func finish(_ locked: Bool = false, code: String = "ok") -> RemoteActionResult {
            var details: [String: Any] = ["locked": locked ? true : "unconfirmed",
                "strategy_used": "shortcut", "path": path, "attempts": attempts]
            if actionSent { details["action_sent"] = true; details["retry_allowed"] = false }
            if let stage { details["stage"] = stage }
            if let lastRead { details["last_read"] = lastRead }
            let finalCode = expired() ? "expired" : code
            return finalCode == "ok" ? RemoteActionResult(details) : RemoteActionResult(code: finalCode, details: details)
        }
        func record(_ entry: String, started: Double, error: String? = nil) {
            var attempt: [String: Any] = ["strategy": "shortcut", "path": path, "entry": entry,
                "ms": Int(max(0, ProcessInfo.processInfo.systemUptime - started) * 1000)]
            if let error { attempt["error_code"] = error }
            attempts.append(attempt)
        }
        func confirm(afterPlay: Bool = false) -> Bool {
            stage = "after_play"
            lastRead = nil
            let end = ProcessInfo.processInfo.systemUptime + 4
            let springBoardPID = afterPlay ? SpikeBridge.remoteSystemPID() : 0
            while !expired(), ProcessInfo.processInfo.systemUptime < end {
                let read = pollRead()
                let marked = read.ok && !unlockLabels(read.snapshot).isDisjoint(with:
                    ["已锁定", "向上轻扫以解锁", "Locked", "Swipe up to unlock"])
                // After the editor's Play tap, a failed AX read is corroborated by
                // remoteForeground; failure alone is not proof of a locked phone.
                let springBoard = springBoardPID > 0 && read.foregroundPID == springBoardPID
                if expired() { return false }
                if ProcessInfo.processInfo.systemUptime <= end, marked || springBoard { return true }
                if !pause(0.3, until: end, expired: expired) { return false }
            }
            return false
        }
        let urlStart = ProcessInfo.processInfo.systemUptime
        // Encode a single query value, including &, +, # and ? in shortcut names.
        let allowed = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~")
        guard let encoded = name.addingPercentEncoding(withAllowedCharacters: allowed),
              let url = URL(string: "shortcuts://run-shortcut?name=\(encoded)") else { return finish(code: "invalid_args") }
        var opened = false
        DispatchQueue.main.sync {
            guard !expired(), UIApplication.shared.applicationState == .active else { return }
            opened = true
            UIApplication.shared.open(url, options: [:], completionHandler: nil)
        }
        actionSent = opened
        record("open_url", started: urlStart, error: opened ? nil : "unsupported")
        if expired() { return finish(code: "expired") }
        if opened, confirm() { return finish(true) }
        if expired() { return finish(code: "expired") }
        path = "ui"
        stage = nil
        lastRead = nil
        guard portrait else { return finish(code: "unsupported_orientation") }
        let launchStart = ProcessInfo.processInfo.systemUptime
        actionSent = true
        let launchError = SpikeBridge.remoteLaunch(bundle: "com.apple.shortcuts")
        record("launch", started: launchStart, error: launchError)
        if expired() { return finish(code: "expired") }
        if let launchError { return finish(code: launchError) }

        func nodes(_ snapshot: [String: Any]) -> [[String: Any]] { snapshot["nodes"] as? [[String: Any]] ?? [] }
        func back(_ snapshot: [String: Any]) -> [String: Any]? {
            nodes(snapshot).first { $0["identifier"] as? String == "BackButton" }
        }
        func editor(_ snapshot: [String: Any]) -> Bool {
            nodes(snapshot).contains {
                ($0["type"] as? NSNumber)?.intValue == 9 &&
                    ["播放", "Play", "Run"].contains($0["label"] as? String ?? "")
            }
        }
        func nodeFrame(_ node: [String: Any]) -> CGRect? {
            guard let frame = node["frame"] as? [String: Any],
                  let x = number(frame["x"]), let y = number(frame["y"]),
                  let w = number(frame["w"]), let h = number(frame["h"]), w > 0, h > 0 else { return nil }
            return CGRect(x: x, y: y, width: w, height: h)
        }
        func card(_ snapshot: [String: Any]) -> [String: Any]? {
            let allNodes = nodes(snapshot)
            let cards = allNodes.filter { ($0["type"] as? NSNumber)?.intValue == 75 }
            if let labeled = cards.first(where: { ($0["label"] as? String ?? "").hasPrefix(name + ", ") }) {
                return labeled
            }
            let textCenters = allNodes.filter {
                ($0["type"] as? NSNumber)?.intValue == 48 && $0["label"] as? String == name
            }.compactMap { nodeFrame($0) }.map { CGPoint(x: $0.midX, y: $0.midY) }
            if let containingText = cards.first(where: { candidate in
                guard let frame = nodeFrame(candidate) else { return false }
                return textCenters.contains { frame.contains($0) }
            }) { return containingText }
            // Use the list's Run button only to locate its card; never tap it.
            for button in allNodes where button["identifier"] as? String == "shortcut.button.run" &&
                ["播放、" + name, "Run " + name].contains(button["label"] as? String ?? "") {
                guard let buttonFrame = nodeFrame(button) else { continue }
                let center = CGPoint(x: buttonFrame.midX, y: buttonFrame.midY)
                if let containingButton = cards.first(where: { nodeFrame($0)?.contains(center) == true }) {
                    return containingButton
                }
            }
            return nil
        }
        func allShortcuts(_ snapshot: [String: Any]) -> [String: Any]? {
            let allNodes = nodes(snapshot)
            guard let text = allNodes.first(where: {
                ($0["type"] as? NSNumber)?.intValue == 48 &&
                    ["所有快捷指令", "All Shortcuts"].contains($0["label"] as? String ?? "")
            }), let frame = nodeFrame(text) else { return nil }
            let center = CGPoint(x: frame.midX, y: frame.midY)
            // Library rows are type 75 too; use the entry's text to locate its row.
            return allNodes.first(where: {
                ($0["type"] as? NSNumber)?.intValue == 75 && nodeFrame($0)?.contains(center) == true
            }) ?? text
        }
        func listReady(_ snapshot: [String: Any]) -> Bool {
            nodes(snapshot).contains {
                ($0["type"] as? NSNumber)?.intValue == 75 ||
                    ["所有快捷指令", "All Shortcuts"].contains($0["label"] as? String ?? "")
            }
        }
        // Wait for launch/navigation/search to settle; reads never trigger more actions.
        // A failed read is transient until the stage's deadline expires.
        func observe(stage waitingStage: String, until predicate: ([String: Any]) -> Bool) -> [String: Any]? {
            stage = waitingStage
            lastRead = nil
            let end = ProcessInfo.processInfo.systemUptime + 4
            let springBoardPID = SpikeBridge.remoteSystemPID()
            while !expired(), ProcessInfo.processInfo.systemUptime < end {
                let read = pollRead()
                if expired() || ProcessInfo.processInfo.systemUptime > end { return nil }
                let pid = (read.snapshot["pid"] as? NSNumber)?.int32Value ?? 0
                if read.ok, pid > 0, read.foregroundPID == pid, pid != springBoardPID,
                   predicate(read.snapshot) { return read.snapshot }
                if !pause(0.3, until: end, expired: expired) { return nil }
            }
            return nil
        }
        func tap(_ node: [String: Any], snapshot: [String: Any], entry: String, xFraction: Double = 0.5) -> String? {
            if expired() { return "expired" }
            guard let frame = nodeFrame(node),
                  let pid = (snapshot["pid"] as? NSNumber)?.int32Value, pid > 0 else { return "target_unknown" }
            var screen = CGRect.zero
            DispatchQueue.main.sync { screen = UIScreen.main.bounds }
            let point = CGPoint(x: frame.minX + frame.width * xFraction, y: frame.midY)
            guard screen.contains(point) else { return "target_unknown" }
            if expired() { return "expired" }
            let started = ProcessInfo.processInfo.systemUptime
            let failure = SpikeBridge.tapScreen(point: point, pid: pid, log: { _ in }) == nil ? nil : "tap_failed"
            record(entry, started: started, error: failure)
            return expired() ? "expired" : failure
        }
        guard var snapshot = observe(stage: "after_launch", until: { listReady($0) || editor($0) || back($0) != nil }) else {
            return finish(code: expired() ? "expired" : "read_failed")
        }
        for _ in 0..<2 {
            guard !editor(snapshot), card(snapshot) == nil, let button = back(snapshot) else { break }
            let previousNodes = NSArray(array: nodes(snapshot))
            if let error = tap(button, snapshot: snapshot, entry: "back") { return finish(code: error) }
            guard let fresh = observe(stage: "library", until: {
                !previousNodes.isEqual(to: nodes($0)) &&
                    (listReady($0) || editor($0) || back($0) != nil)
            }) else { return finish(code: expired() ? "expired" : "read_failed") }
            snapshot = fresh
        }
        if !editor(snapshot) {
            if card(snapshot) == nil, let entry = allShortcuts(snapshot) {
                if let error = tap(entry, snapshot: snapshot, entry: "all_shortcuts") { return finish(code: error) }
                guard let fresh = observe(stage: "list", until: { card($0) != nil }) else {
                    return finish(code: expired() ? "expired" : "shortcut_not_found")
                }
                snapshot = fresh
            }
            if card(snapshot) == nil {
                guard let search = nodes(snapshot).first(where: {
                    ($0["type"] as? NSNumber)?.intValue == 49 || ["搜索", "Search"].contains($0["label"] as? String ?? "")
                }) else { return finish(code: "shortcut_not_found") }
                if let error = tap(search, snapshot: snapshot, entry: "search") { return finish(code: error) }
                guard observe(stage: "list", until: listReady) != nil else { return finish(code: expired() ? "expired" : "read_failed") }
                if expired() { return finish(code: "expired") }
                let typeStart = ProcessInfo.processInfo.systemUptime
                let typed = execute(action: "type", args: ["text": name], depth: 100,
                                    portrait: portrait, status: [:], expired: expired)
                record("type", started: typeStart, error: typed.code == "ok" ? nil : typed.code)
                if typed.code != "ok" { return finish(code: typed.code) }
                guard let fresh = observe(stage: "list", until: { card($0) != nil }) else { return finish(code: expired() ? "expired" : "shortcut_not_found") }
                snapshot = fresh
            }
            guard let selected = card(snapshot) else { return finish(code: "shortcut_not_found") }
            if let error = tap(selected, snapshot: snapshot, entry: "card", xFraction: 0.3) { return finish(code: error) }
            guard let fresh = observe(stage: "editor", until: editor) else { return finish(code: expired() ? "expired" : "read_failed") }
            snapshot = fresh
        }
        guard editor(snapshot) else { return finish(code: "no_effect") }
        guard let play = nodes(snapshot).first(where: {
            ($0["type"] as? NSNumber)?.intValue == 9 && ["播放", "Play", "Run"].contains($0["label"] as? String ?? "")
        }) else { return finish(code: "not_found") }
        // Never repeat this tap, even on failure, an expired deadline or no confirmation.
        if let error = tap(play, snapshot: snapshot, entry: "play") { return finish(code: error) }
        let locked = confirm(afterPlay: true)
        return finish(locked, code: expired() ? "expired" : "ok")
    }
    private func number(_ value: Any?) -> Double? {
        guard let n = value as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(), n.doubleValue.isFinite else { return nil }
        return n.doubleValue
    }
    // Flat AX nodes are in preorder; depth preserves status-bar ancestry.
    private func occluders(_ snapshot: [String: Any], at point: CGPoint, screen: CGRect,
                           targetPID: Int32?, targetFrame: CGRect?) -> [[String: Any]] {
        var statusBarDepth: Int?
        let blockers = (snapshot["nodes"] as? [[String: Any]] ?? []).filter { node in
            let depth = (node["depth"] as? NSNumber)?.intValue ?? 0
            if let ancestor = statusBarDepth, depth <= ancestor { statusBarDepth = nil }
            // XCUIElementTypeStatusBar = 25 (XCUIAutomation/XCUIElementTypes.h).
            if (node["type"] as? NSNumber)?.intValue == 25 || node["type"] as? String == "statusBar" {
                statusBarDepth = depth
            }
            guard statusBarDepth == nil, let frame = node["frame"] as? [String: Any],
                  let x = number(frame["x"]), let y = number(frame["y"]),
                  let w = number(frame["w"]), let h = number(frame["h"]), w > 0, h > 0 else { return false }
            let rect = CGRect(x: x, y: y, width: w, height: h)
            // A same-process container enclosing a label/identifier target is not an overlay.
            if let targetPID, targetPID > 0,
               (snapshot["pid"] as? NSNumber)?.int32Value == targetPID,
               let targetFrame, rect.contains(targetFrame) { return false }
            return rect.intersects(screen) && rect.contains(point) && w * h < screen.width * screen.height * 0.5 &&
                !(y >= 0 && y + h <= 54)
        }.map { ["label": String(($0["label"] as? String ?? "").prefix(120)), "type": $0["type"] ?? 0, "frame": $0["frame"] ?? [:]] }
        return deduplicatedOccluders(blockers)
    }
    private func deduplicatedOccluders(_ nodes: [[String: Any]]) -> [[String: Any]] {
        var result: [[String: Any]] = []
        for node in nodes {
            let frame = node["frame"] as? NSDictionary ?? [:]
            if let index = result.firstIndex(where: { ($0["frame"] as? NSDictionary ?? [:]).isEqual(frame) }) {
                if (result[index]["label"] as? String ?? "").isEmpty,
                   !(node["label"] as? String ?? "").isEmpty { result[index] = node }
            } else { result.append(node) }
        }
        return result
    }
    // Poll deadline during sleeps, without extending a local timeout.
    private func pause(_ seconds: Double, until end: Double, expired: () -> Bool) -> Bool {
        let wake = min(ProcessInfo.processInfo.systemUptime + seconds, end)
        while ProcessInfo.processInfo.systemUptime < wake {
            if expired() { return false }
            Thread.sleep(forTimeInterval: min(0.05, max(0, wake - ProcessInfo.processInfo.systemUptime)))
        }
        return !expired()
    }
    private func foregroundPID() -> Int32? {
        let foreground = SpikeBridge.remoteForeground() as? [String: Any] ?? [:]
        guard let pid = (foreground["pid"] as? NSNumber)?.int32Value, pid > 0 else { return nil }
        return pid
    }
    private func sent(_ failure: String?, expired: () -> Bool) -> RemoteActionResult {
        let details: [String: Any] = ["action_sent": true, "retry_allowed": false]
        if expired() { return RemoteActionResult(code: "expired", details: details) }
        if let failure { return RemoteActionResult(code: failure, details: details) }
        return RemoteActionResult([:])
    }
    // Unlock reads never enter the snapshot cache and never escape in a response.
    private func unlockSnapshot(system: Bool = false) -> [String: Any]? {
        let snapshot = (system ? SpikeBridge.remoteSystemRead(maxDepth: 100) : SpikeBridge.remoteRead(maxDepth: 100)) as? [String: Any] ?? [:]
        guard snapshot["error"] == nil, snapshot["nodes"] is [[String: Any]],
              !isTruncated(snapshot["truncated"]),
              let pid = (snapshot["pid"] as? NSNumber)?.int32Value, pid > 0 else { return nil }
        return snapshot
    }
    private func unlockLabels(_ snapshot: [String: Any]) -> Set<String> {
        Set((snapshot["nodes"] as? [[String: Any]] ?? []).compactMap { $0["label"] as? String })
    }
    private func lockMarked(_ snapshot: [String: Any]) -> Bool {
        !unlockLabels(snapshot).isDisjoint(with: ["已锁定", "向上轻扫以解锁", "密码栏", "Locked", "Swipe up to unlock", "Passcode field"])
    }
    private func keypad(_ snapshot: [String: Any], screen: CGRect) -> [String: CGPoint]? {
        var keys: [String: CGPoint] = [:]
        let nodes = snapshot["nodes"] as? [[String: Any]] ?? []
        for digit in 0...9 {
            let label = String(digit)
            let matches = nodes.filter { $0["label"] as? String == label }
            guard matches.count == 1, let frame = matches[0]["frame"] as? [String: Any],
                  let x = number(frame["x"]), let y = number(frame["y"]),
                  let w = number(frame["w"]), let h = number(frame["h"]), w > 0, h > 0 else { return nil }
            let center = CGPoint(x: x + w / 2, y: y + h / 2)
            guard screen.contains(center) else { return nil }
            keys[label] = center
        }
        return keys
    }
    private func unlock(_ args: [String: Any], portrait: Bool, expired: () -> Bool) -> RemoteActionResult {
        let noRetry: [String: Any] = ["retry_allowed": false]
        func fail(_ code: String) -> RemoteActionResult { RemoteActionResult(code: code, details: noRetry) }
        guard args.keys.allSatisfy({ ["passcode", "timeout_ms"].contains($0) }),
              let passcode = args["passcode"] as? String,
              (4...6).contains(passcode.utf8.count), passcode.utf8.allSatisfy({ (48...57).contains($0) }),
              let ms = number(args["timeout_ms"] ?? 4000), ms >= 0, ms <= 10000, ms.rounded() == ms else { return fail("invalid_args") }
        // Best-effort overwrite of the working digit buffer. Swift/JSON String copies
        // and framework-owned transport memory cannot be reliably zeroized.
        var digits = Array(passcode.utf8)
        defer { for i in digits.indices { digits[i] = 0 } }
        var screen = CGRect.zero
        DispatchQueue.main.sync { screen = UIScreen.main.bounds }
        guard let initial = unlockSnapshot() else { return fail("read_failed") }
        let systemLocked = SpikeBridge.remoteScreenLocked()?.boolValue
        if expired() { return fail("expired") }
        let springPID = SpikeBridge.remoteSystemPID()
        guard springPID > 0, let _ = (initial["pid"] as? NSNumber)?.int32Value else { return fail("target_unknown") }
        // Sleep Focus uses a separate SleepLockScreen process; foreground PID cannot imply unlock.
        let initialLocked = systemLocked == true || lockMarked(initial) || keypad(initial, screen: screen) != nil
        if !initialLocked {
            if systemLocked == false { return RemoteActionResult(["already_unlocked": true]) }
            guard let system = unlockSnapshot(system: true) else { return fail("read_failed") }
            if expired() { return fail("expired") }
            if !lockMarked(system), keypad(system, screen: screen) == nil { return RemoteActionResult(["already_unlocked": true]) }
        }
        guard portrait else { return fail("unsupported_orientation") }
        if expired() { return fail("expired") }
        if SpikeBridge.remotePress(button: "home") != nil { return fail("press_failed") }
        let wakeEnd = ProcessInfo.processInfo.systemUptime + 0.8
        if !pause(0.8, until: wakeEnd, expired: expired) { return fail("expired") }
        guard var snapshot = unlockSnapshot() else { return fail("read_failed") }
        if expired() { return fail("expired") }
        if keypad(snapshot, screen: screen) == nil {
            // Device-provided portrait coordinates (iPhone 14 Pro); do not guess
            // another geometry or perform a second swipe/input attempt.
            let from = CGPoint(x: 196, y: 840), to = CGPoint(x: 196, y: 300)
            guard screen.contains(from), screen.contains(to), let target = (snapshot["pid"] as? NSNumber)?.int32Value else { return fail("target_unknown") }
            if SpikeBridge.swipeScreen(from: from, to: to, pid: target, duration: 0.3) != nil { return fail("swipe_failed") }
        }
        let keypadEnd = ProcessInfo.processInfo.systemUptime + 3
        while keypad(snapshot, screen: screen) == nil {
            if expired() { return fail("expired") }
            if ProcessInfo.processInfo.systemUptime >= keypadEnd { return fail("keypad_not_found") }
            if !pause(0.3, until: keypadEnd, expired: expired) { return fail("expired") }
            guard let fresh = unlockSnapshot() else { return fail("keypad_not_found") }
            snapshot = fresh
        }
        for i in digits.indices {
            if expired() { return fail("expired") }
            guard let keys = keypad(snapshot, screen: screen), let point = keys[String(Int(digits[i]) - 48)],
                  let target = (snapshot["pid"] as? NSNumber)?.int32Value else { return fail("unlock_unconfirmed") }
            // Deliberately bypass general tap occlusion logic. Never return bridge text.
            if SpikeBridge.tapScreen(point: point, pid: target, log: { _ in }) != nil { return fail("tap_failed") }
            if i < digits.count - 1 {
                let next = ProcessInfo.processInfo.systemUptime + 0.12
                if !pause(0.12, until: next, expired: expired) { return fail("expired") }
            }
        }
        let end = ProcessInfo.processInfo.systemUptime + ms / 1000
        while true {
            if expired() { return fail("expired") }
            guard let fresh = unlockSnapshot(), let system = unlockSnapshot(system: true) else { return fail("unlock_unconfirmed") }
            if expired() { return fail("expired") }
            let keyboard = keypad(fresh, screen: screen) != nil || keypad(system, screen: screen) != nil
            let numericLabels = Set((0...9).map { String($0) })
            let uncertainKeyboard = numericLabels.isSubset(of: unlockLabels(fresh)) || numericLabels.isSubset(of: unlockLabels(system))
            if !keyboard, !uncertainKeyboard, !lockMarked(fresh), !lockMarked(system), SpikeBridge.remoteScreenLocked()?.boolValue != true { return RemoteActionResult(["unlocked": true]) }
            if ProcessInfo.processInfo.systemUptime >= end { return fail(keyboard ? "unlock_failed" : "unlock_unconfirmed") }
            if !pause(0.3, until: end, expired: expired) { return fail("expired") }
        }
    }
    func execute(action: String, args: [String: Any], depth: Int, portrait: Bool,
                 status: [String: Any], expired: () -> Bool) -> RemoteActionResult {
        let requestStarted = ProcessInfo.processInfo.systemUptime
        if expired() { return RemoteActionResult(code: "expired") }
        switch action {
        case "unlock":
            return unlock(args, portrait: portrait, expired: expired)
        case "status":
            var result = status
            result["foreground"] = (status["xctest_ready"] as? Bool == true) ? SpikeBridge.remoteForeground() : ["pid": 0, "bundle": "unknown"]
            return RemoteActionResult(result)
        case "read":
            var requested = depth
            if args["max_depth"] != nil {
                guard let n = number(args["max_depth"]), n >= 1, n <= 100, n.rounded() == n else { return RemoteActionResult(code: "invalid_args") }
                requested = Int(n)
            }
            guard args["target"] == nil || ["foreground", "system"].contains(args["target"] as? String ?? "") else {
                return RemoteActionResult(code: "invalid_args")
            }
            if args["target"] as? String == "system" {
                let system = SpikeBridge.remoteSystemRead(maxDepth: requested) as? [String: Any] ?? [:]
                if expired() { return RemoteActionResult(code: "expired") }
                guard system["error"] == nil, system["nodes"] != nil else { return RemoteActionResult(code: "read_failed") }
                return RemoteActionResult(system)
            }
            return read(requested)
        case "screenshot":
            guard args["format"] == nil || args["format"] as? String == "png" else { return RemoteActionResult(code: "unsupported") }
            var error: NSString?
            guard let data = SpikeBridge.screenshot(log: { _ in }, error: &error),
                  data.starts(with: [137, 80, 78, 71, 13, 10, 26, 10]),
                  let source = CGImageSourceCreateWithData(data as CFData, nil),
                  let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
                  let width = properties[kCGImagePropertyPixelWidth], let height = properties[kCGImagePropertyPixelHeight] else {
                return RemoteActionResult(code: "screenshot_failed")
            }
            guard data.count <= 12 * 1024 * 1024 - 4096 else { return RemoteActionResult(code: "frame_too_large") }
            return RemoteActionResult(["png": data.base64EncodedString(), "width": width, "height": height])
        case "type":
            guard let text = args["text"] as? String, text.utf16.count <= 1000 else { return RemoteActionResult(code: "invalid_args") }
            let snapshot = SpikeBridge.remoteInputRead(maxDepth: 100) as? [String: Any] ?? [:]
            if expired() { return RemoteActionResult(code: "expired") }
            guard snapshot["error"] == nil, let nodes = snapshot["nodes"] as? [[String: Any]] else { return RemoteActionResult(code: "read_failed") }
            // No focus check: on device AX focus reads false while typing and the keyboard
            // lives out of process, so neither can prove the absence of focus.
            _ = nodes
            guard let pid = foregroundPID(), (snapshot["pid"] as? NSNumber)?.int32Value == pid else { return RemoteActionResult(code: "target_unknown") }
            if expired() { return RemoteActionResult(code: "expired") }
            return sent(SpikeBridge.remoteType(text: text, pid: pid), expired: expired)
        case "swipe":
            guard portrait else { return RemoteActionResult(code: "unsupported_orientation") }
            guard let from = args["from"] as? [String: Any], let to = args["to"] as? [String: Any],
                  let fx = number(from["x"]), let fy = number(from["y"]),
                  let tx = number(to["x"]), let ty = number(to["y"]),
                  let ms = number(args["duration_ms"] ?? 300), ms >= 50, ms <= 5000, ms.rounded() == ms else { return RemoteActionResult(code: "invalid_args") }
            var screen = CGRect.zero
            DispatchQueue.main.sync { screen = UIScreen.main.bounds }
            guard screen.contains(CGPoint(x: fx, y: fy)), screen.contains(CGPoint(x: tx, y: ty)) else { return RemoteActionResult(code: "invalid_args") }
            guard let pid = foregroundPID() else { return RemoteActionResult(code: "target_unknown") }
            if expired() { return RemoteActionResult(code: "expired") }
            let failure = SpikeBridge.swipeScreen(from: CGPoint(x: fx, y: fy), to: CGPoint(x: tx, y: ty), pid: pid, duration: ms / 1000)
            // Existing swipe errors may include private exception strings; never return/log them.
            let code = failure.map { $0.hasPrefix("MissingPrivateAPI:") ? "unsupported" : "swipe_failed" }
            return sent(code, expired: expired)
        case "press":
            guard let button = args["button"] as? String, ["home", "volume_up", "volume_down", "lock"].contains(button) else { return RemoteActionResult(code: "invalid_args") }
            let strategy = args["strategy"] as? String ?? "auto"
            guard args["strategy"] == nil || args["strategy"] is String,
                  ["auto", "shortcut", "hid", "device_event", "press_lock"].contains(strategy),
                  button == "lock" || (args["strategy"] == nil && args["shortcut_name"] == nil) else { return RemoteActionResult(code: "invalid_args") }
            if button != "lock" { return sent(SpikeBridge.remotePress(button: button), expired: expired) }
            guard args["shortcut_name"] == nil || args["shortcut_name"] is String else { return RemoteActionResult(code: "invalid_args") }
            let name = args["shortcut_name"] as? String ?? "锁屏"
            guard !name.isEmpty, name.utf16.count <= 50 else { return RemoteActionResult(code: "invalid_args") }
            if ["auto", "shortcut"].contains(strategy) { return shortcutLock(name: name, portrait: portrait, expired: expired) }
            return lock(strategy: strategy, expired: expired)
        case "launch":
            guard let bundle = args["bundle_id"] as? String, !bundle.isEmpty, bundle.utf8.count <= 255,
                  bundle.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 46].contains($0) }) else { return RemoteActionResult(code: "invalid_args") }
            if bundle == Bundle.main.bundleIdentifier {
                var active = false
                DispatchQueue.main.sync { active = UIApplication.shared.applicationState == .active }
                if expired() { return RemoteActionResult(code: "expired") }
                if active { return RemoteActionResult(["pid": Int(getpid()), "bundle": bundle, "confirmed_by": "application_state"]) }
                // Info.plist registers no self URL scheme; UIKit open is unavailable in background.
                return RemoteActionResult(code: "unsupported", details: ["message":
                    "Self Runner is not active; no self URL scheme or supported background foregrounding API is configured. Open Self Runner manually. LSApplicationWorkspace self-launch returned false on device (2026-10-13); its system cause is unverified."])
            }
            guard let beforePID = foregroundPID() else { return RemoteActionResult(code: "target_unknown") }
            let springBoardPID = SpikeBridge.remoteSystemPID()
            if expired() { return RemoteActionResult(code: "expired") }
            let end = ProcessInfo.processInfo.systemUptime + 3
            let outcome = sent(SpikeBridge.remoteLaunch(bundle: bundle), expired: expired)
            guard outcome.code == "ok" else { return outcome }
            // Give Launch Services time to transition before accepting an unchanged PID.
            if !pause(0.3, until: end, expired: expired) { return sent(nil, expired: expired) }
            while true {
                if expired() { return sent(nil, expired: expired) }
                let foreground = SpikeBridge.remoteForeground() as? [String: Any] ?? [:]
                if expired() { return sent(nil, expired: expired) }
                let pid = (foreground["pid"] as? NSNumber)?.int32Value ?? 0
                let observedBundle = foreground["bundle"] as? String ?? "unknown"
                var confirmedBy: String?
                if pid > 0, observedBundle == bundle {
                    confirmedBy = "bundle"
                } else if pid > 0, springBoardPID > 0, pid != springBoardPID, pid != beforePID {
                    confirmedBy = "pid_change"
                } else if ProcessInfo.processInfo.systemUptime >= end, pid == beforePID,
                          observedBundle.isEmpty || observedBundle == "unknown" {
                    confirmedBy = "unchanged_unverified"
                }
                if let confirmedBy {
                    return RemoteActionResult(["pid": pid, "bundle": observedBundle, "confirmed_by": confirmedBy])
                }
                if ProcessInfo.processInfo.systemUptime >= end { return RemoteActionResult(code: "foreground_unconfirmed", details: ["action_sent": true, "retry_allowed": false, "foreground": foreground]) }
                if !pause(0.3, until: end, expired: expired) { return sent(nil, expired: expired) }
            }
        case "open_url":
            guard let text = args["url"] as? String, text.utf16.count <= 2048,
                  !text.unicodeScalars.contains(where: { CharacterSet.whitespacesAndNewlines.union(.controlCharacters).contains($0) }),
                  let url = URL(string: text), let scheme = url.scheme, !scheme.isEmpty,
                  !["http", "https"].contains(scheme.lowercased()) || !(url.host ?? "").isEmpty else { return RemoteActionResult(code: "invalid_args") }
            let guidance = "请改用 launch Safari + tap 地址栏 + type url\n"
            let done = DispatchSemaphore(value: 0)
            var opened = false
            var attempted = false
            DispatchQueue.main.sync {
                guard !expired(), UIApplication.shared.applicationState == .active else { return }
                attempted = true
                UIApplication.shared.open(url, options: [:]) { success in
                    opened = success
                    done.signal()
                }
            }
            if !attempted {
                if expired() { return RemoteActionResult(code: "expired") }
                return RemoteActionResult(code: "unsupported", details: ["message": guidance])
            }
            // Completion publishes its result through the semaphore; poll the deadline.
            while done.wait(timeout: .now() + 0.05) == .timedOut {
                if expired() { return sent(nil, expired: expired) }
            }
            if expired() { return sent(nil, expired: expired) }
            guard opened else {
                return RemoteActionResult(code: "unsupported", details: ["message": guidance, "action_sent": true, "retry_allowed": false])
            }
            let end = ProcessInfo.processInfo.systemUptime + 0.3
            if !pause(0.3, until: end, expired: expired) { return sent(nil, expired: expired) }
            let foreground = SpikeBridge.remoteForeground() as? [String: Any] ?? [:]
            if expired() { return sent(nil, expired: expired) }
            return RemoteActionResult(foreground)
        case "wait_for":
            let keys = ["label", "identifier"].filter { args[$0] != nil }
            guard keys.count == 1, let key = keys.first, let wanted = args[key] as? String,
                  let ms = number(args["timeout_ms"] ?? 5000), ms >= 0, ms <= 30000, ms.rounded() == ms else { return RemoteActionResult(code: "invalid_args") }
            let end = ProcessInfo.processInfo.systemUptime + ms / 1000
            while true {
                if expired() { return RemoteActionResult(code: "expired") }
                let fresh = read(depth)
                if expired() { return RemoteActionResult(code: "expired") }
                guard fresh.code == "ok" else { return fresh }
                if let node = (fresh.result?["nodes"] as? [[String: Any]] ?? []).first(where: { $0[key] as? String == wanted }) {
                    return RemoteActionResult(["found": true, "node": node])
                }
                if ProcessInfo.processInfo.systemUptime >= end { return RemoteActionResult(["found": false]) }
                if !pause(0.3, until: end, expired: expired) { return RemoteActionResult(code: "expired") }
            }
        case "tap":
            guard portrait else { return RemoteActionResult(code: "unsupported_orientation") }
            let coordinate = args["x"] != nil || args["y"] != nil
            let label = args["label"] != nil, identifier = args["identifier"] != nil
            guard [coordinate, label, identifier].filter({ $0 }).count == 1 else { return RemoteActionResult(code: "invalid_args") }
            let expectKeys = ["expect_label", "expect_identifier"].filter { args[$0] != nil }
            guard expectKeys.count <= 1, expectKeys.allSatisfy({ args[$0] is String }) else { return RemoteActionResult(code: "invalid_args") }
            let waitMS = number(args["wait_occlusion_ms"] ?? 6000)
            let expectMS = number(args["expect_timeout_ms"] ?? 3000)
            guard let waitMS, waitMS >= 0, waitMS.rounded() == waitMS,
                  let expectMS, expectMS >= 0, expectMS <= 15000, expectMS.rounded() == expectMS else { return RemoteActionResult(code: "invalid_args") }
            var screen = CGRect.zero
            DispatchQueue.main.sync { screen = UIScreen.main.bounds }
            let occlusionEnd = ProcessInfo.processInfo.systemUptime + waitMS / 1000
            var dismissalEnd: Double?
            var observedOccluders: [[String: Any]] = []
            var swipeError: String?
            var detectedAt: Double?
            var clearedAt: Double?
            var clearedBy = "none"
            var swipeAttempted = false
            var relocated = false
            var relocationPending = false
            func finish(_ outcome: RemoteActionResult) -> RemoteActionResult {
                guard let detectedAt else { return outcome }
                var outcome = outcome
                var details = outcome.result ?? [:]
                details["occluders"] = observedOccluders
                details["occlusion"] = [
                    "detected_ms_into_request": Int(max(0, detectedAt - requestStarted) * 1000),
                    "waited_ms": Int(max(0, (clearedAt ?? ProcessInfo.processInfo.systemUptime) - detectedAt) * 1000),
                    "cleared_by": clearedBy, "swipe_attempted": swipeAttempted,
                    "relocated": relocated, "occluders": observedOccluders
                ] as [String: Any]
                outcome.result = details
                return outcome
            }
            var expected: Int?
            if args["expect_pid"] != nil {
                guard let n = number(args["expect_pid"]), n > 0, n <= Double(Int32.max), n.rounded() == n else { return finish(RemoteActionResult(code: "invalid_args")) }
                expected = Int(n)
            }
            var snapshot: [String: Any]?
            if args["snapshot_id"] != nil {
                guard let id = args["snapshot_id"] as? String else { return finish(RemoteActionResult(code: "invalid_args")) }
                snapshot = snapshots.first { $0["snapshot_id"] as? String == id }
                guard snapshot != nil else { return finish(RemoteActionResult(code: "stale_target")) }
            }
            var point: CGPoint
            var matched: [String: Any]?
            var targetFrame: CGRect?
            while true {
                if expired() { return finish(RemoteActionResult(code: "expired")) }

                if coordinate {
                    guard let x = number(args["x"]), let y = number(args["y"]), args["index"] == nil else { return finish(RemoteActionResult(code: "invalid_args")) }
                    point = CGPoint(x: x, y: y)
                } else {
                    if snapshot == nil {
                        let fresh = readForTap(depth, expired: expired)
                        if expired() { return finish(RemoteActionResult(code: "expired")) }
                        guard fresh.code == "ok" else { return finish(fresh) }
                        snapshot = fresh.result
                    }
                    guard !isTruncated(snapshot?["truncated"]) else { return finish(RemoteActionResult(code: "snapshot_truncated")) }
                    let key = label ? "label" : "identifier"
                    guard let text = args[key] as? String else { return finish(RemoteActionResult(code: "invalid_args")) }
                    let candidates = (snapshot?["nodes"] as? [[String: Any]] ?? []).filter { $0[key] as? String == text }
                    guard !candidates.isEmpty else { return finish(RemoteActionResult(code: "not_found")) }
                    let selected: [String: Any]
                    if args["index"] != nil {
                        guard let n = number(args["index"]), n >= 0, n.rounded() == n, n < Double(candidates.count) else { return finish(RemoteActionResult(code: "not_found")) }
                        selected = candidates[Int(n)]
                    } else {
                        guard candidates.count == 1 else { return finish(RemoteActionResult(code: "ambiguous", details: ["candidates": candidates.count])) }
                        selected = candidates[0]
                    }
                    guard let frame = selected["frame"] as? [String: Any], let x = number(frame["x"]), let y = number(frame["y"]),
                          let w = number(frame["w"]), let h = number(frame["h"]), w > 0, h > 0 else { return finish(RemoteActionResult(code: "target_unknown")) }
                    point = CGPoint(x: x + w / 2, y: y + h / 2)
                    targetFrame = CGRect(x: x, y: y, width: w, height: h)
                    matched = ["label": selected["label"] ?? "", "identifier": selected["identifier"] ?? "", "type": selected["type"] ?? 0]
                }
                if relocationPending && !coordinate { relocated = true; relocationPending = false }
                let targetPID = (snapshot?["pid"] as? NSNumber)?.int32Value ?? foregroundPID()
                var encountered = false
                while true {
                    if expired() { return finish(RemoteActionResult(code: "expired")) }
                    let system = SpikeBridge.remoteSystemRead(maxDepth: 100) as? [String: Any] ?? [:]
                    if expired() { return finish(RemoteActionResult(code: "expired")) }
                    guard system["error"] == nil, system["nodes"] != nil else { return finish(RemoteActionResult(code: "read_failed")) }
                    // SpringBoard's own target tree must not occlude its controls, even if truncated.
                    if let targetPID, targetPID > 0,
                       (system["pid"] as? NSNumber)?.int32Value == targetPID { break }
                    let blockers = occluders(system, at: point, screen: screen,
                                             targetPID: targetPID, targetFrame: targetFrame)
                    if !blockers.isEmpty {
                        if detectedAt == nil { detectedAt = ProcessInfo.processInfo.systemUptime }
                        clearedAt = nil
                        clearedBy = "none"
                        observedOccluders = deduplicatedOccluders(observedOccluders + blockers)
                        encountered = true
                    }
                    // A partial system tree cannot establish that this point is clear.
                    if blockers.isEmpty {
                        guard !isTruncated(system["truncated"]) else { return finish(RemoteActionResult(code: "snapshot_truncated")) }
                        if encountered {
                            clearedAt = ProcessInfo.processInfo.systemUptime
                            clearedBy = swipeAttempted ? "swipe" : "auto_dismiss"
                        }
                        break
                    }
                    let banner = blockers.filter {
                        let frame = $0["frame"] as? [String: Any] ?? [:]
                        return (number(frame["y"]) ?? screen.height) < screen.height / 3
                    }.max { a, b in
                        let af = a["frame"] as? [String: Any] ?? [:], bf = b["frame"] as? [String: Any] ?? [:]
                        return (number(af["w"]) ?? 0) * (number(af["h"]) ?? 0) < (number(bf["w"]) ?? 0) * (number(bf["h"]) ?? 0)
                    }
                    // Non-top blockers only wait; a top banner gets one early swipe.
                    let end: Double
                    if let dismissalEnd {
                        end = max(dismissalEnd, occlusionEnd)
                    } else if banner != nil && !swipeAttempted {
                        end = min(occlusionEnd, (detectedAt ?? requestStarted) + 1.5)
                    } else { end = occlusionEnd }
                    if ProcessInfo.processInfo.systemUptime < end {
                        if !pause(0.5, until: end, expired: expired) { return finish(RemoteActionResult(code: "expired")) }
                        continue
                    }
                    if !swipeAttempted, let banner,
                       let frame = banner["frame"] as? [String: Any],
                       let y = number(frame["y"]), let x = number(frame["x"]),
                       let w = number(frame["w"]), let h = number(frame["h"]),
                       let systemPID = (system["pid"] as? NSNumber)?.int32Value, systemPID > 0 {
                        if expired() { return finish(RemoteActionResult(code: "expired")) }
                        swipeAttempted = true
                        let failure = SpikeBridge.swipeScreen(from: CGPoint(x: x + w / 2, y: y + h / 2),
                            to: CGPoint(x: x + w / 2, y: max(0, y - h)), pid: systemPID, log: { _ in })
                        if expired() { return finish(RemoteActionResult(code: "expired")) }
                        dismissalEnd = ProcessInfo.processInfo.systemUptime + 1.5
                        swipeError = failure
                        continue
                    }
                    var details: [String: Any] = ["occluders": blockers]
                    if let swipeError { details["swipe_error"] = swipeError }
                    return finish(RemoteActionResult(code: "occluded", details: details))
                }
                if encountered {
                    // A banner may have shifted the foreground tree; never reuse its old center.
                    let fresh = args["snapshot_id"] == nil && !coordinate
                        ? readForTap(depth, expired: expired) : read(depth)
                    if expired() { return finish(RemoteActionResult(code: "expired")) }
                    guard fresh.code == "ok" else { return finish(fresh) }
                    if let oldPID = snapshot?["pid"] as? NSNumber,
                       (fresh.result?["pid"] as? NSNumber)?.intValue != oldPID.intValue { return finish(RemoteActionResult(code: "stale_target")) }
                    snapshot = fresh.result
                    relocationPending = true
                    continue
                }
                break
            }
            let pointPID = SpikeBridge.remotePID(at: point) as? [String: Any] ?? [:]
            if expired() { return finish(RemoteActionResult(code: "expired")) }
            var pid = (pointPID["pid"] as? NSNumber)?.int32Value ?? 0
            var pidSource = "ax_point"
            if pid <= 0 {
                if let snapshotPID = (snapshot?["pid"] as? NSNumber)?.int32Value, snapshotPID > 0,
                   let foreground = SpikeBridge.remoteForeground() as? [String: Any],
                   (foreground["pid"] as? NSNumber)?.int32Value == snapshotPID {
                    pid = snapshotPID
                    pidSource = "foreground_fallback"
                } else {
                    if expired() { return finish(RemoteActionResult(code: "expired")) }
                    return finish(RemoteActionResult(code: "target_unknown", details: ["details": pointPID]))
                }
            }
            if expired() { return finish(RemoteActionResult(code: "expired")) }
            if let expected, expected != Int(pid) { return finish(RemoteActionResult(code: "stale_target")) }
            if let snapshot, (snapshot["pid"] as? NSNumber)?.intValue != Int(pid) { return finish(RemoteActionResult(code: "stale_target")) }
            var stillPortrait = false
            DispatchQueue.main.sync {
                stillPortrait = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first?.interfaceOrientation == .portrait
            }
            guard stillPortrait else { return finish(RemoteActionResult(code: "unsupported_orientation")) }
            if expired() { return finish(RemoteActionResult(code: "expired")) }
            guard SpikeBridge.tapScreen(point: point, pid: pid, log: { _ in }) == nil else { return finish(RemoteActionResult(code: "tap_failed")) }
            var result: [String: Any] = ["pid": pid, "pid_source": pidSource, "point": ["x": point.x, "y": point.y]]
            if let matched { result["matched"] = matched }
            if pidSource == "foreground_fallback" { result["ax_point_error"] = pointPID }
            if !observedOccluders.isEmpty { result["occluders"] = observedOccluders }
            if let expectKey = expectKeys.first {
                result["tap_sent"] = true
                result["retry_allowed"] = false
                let end = ProcessInfo.processInfo.systemUptime + expectMS / 1000
                while true {
                    if expired() { return finish(RemoteActionResult(code: "expired", details: result)) }
                    let fresh = read(depth)
                    if expired() { return finish(RemoteActionResult(code: "expired", details: result)) }
                    let key = expectKey == "expect_label" ? "label" : "identifier"
                    if (fresh.result?["nodes"] as? [[String: Any]] ?? []).contains(where: { $0[key] as? String == args[expectKey] as? String }) {
                        result["expect"] = "met"
                        return finish(RemoteActionResult(result))
                    }
                    if ProcessInfo.processInfo.systemUptime >= end {
                        result["expect"] = "timeout"
                        return finish(RemoteActionResult(code: "no_effect", details: result))
                    }
                    if !pause(0.3, until: end, expired: expired) { return finish(RemoteActionResult(code: "expired", details: result)) }
                }
            }
            return finish(RemoteActionResult(result))
        default: return RemoteActionResult(code: "unsupported")
        }
    }
}
