import Foundation
import Darwin

enum NativeHost {
    private static let credentialLock = NSLock()
    private static var pendingRunnerToken: String?
    private static var credentialCheckInFlight = false
    private static var credentialState = "unchanged"
    private typealias Start = @convention(c) (UnsafePointer<CChar>?) -> Int32
    private typealias Stop = @convention(c) () -> Void
    private typealias Status = @convention(c) () -> UnsafeMutablePointer<CChar>?
    private typealias Free = @convention(c) (UnsafeMutablePointer<CChar>?) -> Void
    private typealias CopyRecord = @convention(c) (UnsafeMutablePointer<UInt8>?, Int) -> Int
    private static func symbol<T>(_ name: String, as: T.Type) -> T? {
        guard let pointer = dlsym(UnsafeMutableRawPointer(bitPattern: -2), name) else { return nil }
        return unsafeBitCast(pointer, to: T.self)
    }
    static var linked: Bool { symbol("ipu_host_start", as: Start.self) != nil }
    static func status() -> [String: Any] {
        guard let status = symbol("ipu_host_status", as: Status.self),
              let free = symbol("ipu_host_string_free", as: Free.self), let value = status() else {
            return ["phase": "not_linked", "linked": false]
        }
        defer { free(value) }
        let data = Data(String(cString: value).utf8)
        var result = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? ["phase": "invalid_status"]
        result["linked"] = true
        verifyRunnerCredential(phase: result["phase"] as? String ?? "unknown")
        credentialLock.lock()
        result["runner_credential_state"] = credentialState
        credentialLock.unlock()
        // Persist only via the private ABI, never through report/status serialization.
        if result["pair_record_available"] as? Bool == true,
           let copy = symbol("ipu_host_copy_pair_record", as: CopyRecord.self) {
            let size = copy(nil, 0)
            if size > 0 && size < 65_536 {
                var data = Data(count: size)
                let copied = data.withUnsafeMutableBytes { copy($0.bindMemory(to: UInt8.self).baseAddress, size) }
                if copied == size {
                    do { if ProtectedStore.read("pair_record") != data { try ProtectedStore.write("pair_record", data) } }
                    catch { result["storage_error"] = "pair_record_save_failed" }
                }
                data.resetBytes(in: 0..<data.count)
            }
        }
        return result
    }
    static func start(mode: String, bridgePort: UInt16, bridgeToken: String) -> String? {
        guard let start = symbol("ipu_host_start", as: Start.self) else { return "native_host_not_linked" }
        do {
            let runnerToken = mode == "launch" ? try ProtectedStore.freshToken() : try ProtectedStore.token("runner_token")
            var config: [String: Any] = ["mode": mode, "device_host": "10.7.0.1", "bridge_port": bridgePort,
                "bridge_token": bridgeToken, "runner_bundle": Bundle.main.object(forInfoDictionaryKey: "IPURunnerBundle") as? String ?? "com.leeguoo.iphone-use.selfhost-runner.xctrunner",
                "runner_token": runnerToken,
                "developer_image_root": Bundle.main.bundleURL.appendingPathComponent("DeveloperImage").path,
                "daemon_token": try ProtectedStore.token("daemon_token"),
                "daemon_password": try ProtectedStore.token("daemon_password")]
            if let record = ProtectedStore.read("pair_record") { config["pair_record_base64"] = record.base64EncodedString() }
            let data = try JSONSerialization.data(withJSONObject: config)
            guard let encoded = String(data: data, encoding: .utf8) else { return "native_config_invalid" }
            let code = encoded.withCString { start($0) }
            if code == 0 && mode == "launch" {
                credentialLock.lock()
                pendingRunnerToken = runnerToken
                credentialCheckInFlight = false
                credentialState = "pending"
                credentialLock.unlock()
            }
            return code == 0 ? nil : (code == 1 ? "native_host_already_active" : "native_config_invalid")
        } catch { return "keychain_unavailable" }
    }
    private static func verifyRunnerCredential(phase: String) {
        credentialLock.lock()
        if ["failed", "stopped", "runner_exited"].contains(phase) {
            pendingRunnerToken = nil
            credentialCheckInFlight = false
            credentialState = phase
        }
        guard phase == "running", let token = pendingRunnerToken, !credentialCheckInFlight else {
            credentialLock.unlock(); return
        }
        credentialCheckInFlight = true
        credentialLock.unlock()
        var request = URLRequest(url: URL(string: "http://127.0.0.1:18100/status")!)
        request.timeoutInterval = 5
        request.setValue(RunnerSigning.authorization(token: token, method: "GET", target: "/status",
            timestamp: Int64(Date().timeIntervalSince1970), nonce: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(), body: Data()), forHTTPHeaderField: "Authorization")
        URLSession.shared.dataTask(with: request) { data, response, _ in
            credentialLock.lock()
            defer { credentialLock.unlock() }
            guard pendingRunnerToken == token else { return }
            credentialCheckInFlight = false
            let object = data.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]
            guard (response as? HTTPURLResponse)?.statusCode == 200,
                  (object?["value"] as? [String: Any])?["ready"] as? Bool == true else {
                credentialState = "candidate_not_serving"; return
            }
            do {
                try ProtectedStore.write("runner_token", Data(token.utf8))
                pendingRunnerToken = nil
                credentialState = "verified"
            } catch { credentialState = "save_failed" }
        }.resume()
    }
    static func stop() {
        credentialLock.lock()
        pendingRunnerToken = nil
        credentialCheckInFlight = false
        credentialState = "stopped"
        credentialLock.unlock()
        symbol("ipu_host_stop", as: Stop.self)?()
    }
    static func stopGracefully(completion: @escaping () -> Void) {
        credentialLock.lock()
        pendingRunnerToken = nil
        credentialCheckInFlight = false
        credentialState = "stopping"
        credentialLock.unlock()
        guard let data = ProtectedStore.read("runner_token"), let token = String(data: data, encoding: .utf8) else {
            stop(); completion(); return
        }
        var request = URLRequest(url: URL(string: "http://127.0.0.1:18100/shutdown")!)
        request.httpMethod = "POST"
        request.httpBody = Data()
        request.timeoutInterval = 3
        request.setValue(RunnerSigning.authorization(token: token, method: "POST", target: "/shutdown",
            timestamp: Int64(Date().timeIntervalSince1970), nonce: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(), body: Data()), forHTTPHeaderField: "Authorization")
        URLSession.shared.dataTask(with: request) { _, _, _ in
            stop()
            completion()
        }.resume()
    }
}
