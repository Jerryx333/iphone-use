import Foundation
import SwiftUI
import UIKit
import CryptoKit

struct PhoneRunnerClient {
    struct Failure: Error { let code: String }
    func request(_ method: String, _ target: String, body: [String: Any]? = nil) async throws -> [String: Any] {
        guard let saved = ProtectedStore.read("runner_token"), let token = String(data: saved, encoding: .utf8) else { throw Failure(code: "credential_missing") }
        let bytes = try body.map { try JSONSerialization.data(withJSONObject: $0, options: [.sortedKeys]) } ?? Data()
        var request = URLRequest(url: URL(string: "http://127.0.0.1:18100" + target)!)
        request.httpMethod = method
        request.timeoutInterval = 20
        if method != "GET" { request.httpBody = bytes; request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        let nonce = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        request.setValue(RunnerSigning.authorization(token: token, method: method, target: target,
            timestamp: Int64(Date().timeIntervalSince1970), nonce: nonce, body: bytes), forHTTPHeaderField: "Authorization")
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else { throw Failure(code: "runner_request_failed") }
        guard data.count <= 8_388_608, let value = (try JSONSerialization.jsonObject(with: data)) as? [String: Any] else { throw Failure(code: "invalid_runner_response") }
        return value
    }
    func ownedSource() async throws -> [String: Any] {
        let app = try await request("GET", "/apps/active")
        guard let active = app["value"] as? [String: Any], active["bundleId"] as? String == "com.leeguoo.iphone-use.selfhost-probe" else { throw Failure(code: "validation_app_not_foreground") }
        let response = try await request("GET", "/source?format=json")
        if let tree = response["value"] as? [String: Any] { return tree }
        if let text = response["value"] as? String,
           let tree = (try JSONSerialization.jsonObject(with: Data(text.utf8))) as? [String: Any] { return tree }
        throw Failure(code: "source_unavailable")
    }
    static func node(_ identifier: String, in tree: [String: Any]) -> [String: Any]? {
        if tree["rawIdentifier"] as? String == identifier || tree["name"] as? String == identifier { return tree }
        for child in tree["children"] as? [[String: Any]] ?? [] {
            if let result = node(identifier, in: child) { return result }
        }
        return nil
    }
    func tap(_ identifier: String) async throws {
        let tree = try await ownedSource()
        guard let node = Self.node(identifier, in: tree), let rect = node["rect"] as? [String: Any],
              let x = rect["x"] as? Double, let y = rect["y"] as? Double,
              let width = rect["width"] as? Double, let height = rect["height"] as? Double,
              width > 0, height > 0 else { throw Failure(code: "validation_target_missing") }
        _ = try await request("POST", "/tap", body: ["x": x + width/2, "y": y + height/2])
    }
}

@MainActor final class ControlSmoke: ObservableObject {
    @Published var count = 0
    @Published var text = ""
    @Published var phase = "未验证"
    @Published var running = false
    @Published var image: UIImage?
    @Published var reportURL: URL?
    func run() async {
        guard !running else { return }
        running = true; count = 0; text = ""; image = nil; reportURL = nil
        let started = Date()
        let challenge = "selfhost-" + UUID().uuidString.prefix(8).lowercased()
        var report: [String: Any] = ["schema": 1, "executor": "on-device app via signed local runner HTTP",
            "started_at": ISO8601DateFormatter().string(from: started),
            "scope": "real input/source/screenshot in this test app; Mac disconnection requires a separate recorded observation"]
        defer { running = false }
        do {
            phase = "检查独立服务"
            let client = PhoneRunnerClient()
            let status = try await client.request("GET", "/status")
            guard let value = status["value"] as? [String: Any], let daemon = value["daemon"] as? [String: Any], daemon["state"] as? String == "serving" else {
                throw PhoneRunnerClient.Failure(code: "embedded_daemon_not_serving")
            }
            phase = "验证真实点击"
            try await Task.sleep(nanoseconds: 300_000_000)
            try await client.tap("selfhost-smoke-button")
            for _ in 0..<20 {
                if count == 1 { break }
                try await Task.sleep(nanoseconds: 100_000_000)
            }
            guard count == 1 else { throw PhoneRunnerClient.Failure(code: "tap_not_observed") }
            report["tap_observed"] = true
            phase = "验证真实输入"
            try await client.tap("selfhost-smoke-text")
            _ = try await client.request("POST", "/type", body: ["text": challenge])
            for _ in 0..<20 {
                if text == challenge { break }
                try await Task.sleep(nanoseconds: 100_000_000)
            }
            guard text == challenge else { throw PhoneRunnerClient.Failure(code: "typing_not_observed") }
            let tree = try await client.ownedSource()
            guard PhoneRunnerClient.node("selfhost-smoke-text", in: tree)?["value"] as? String == challenge else {
                throw PhoneRunnerClient.Failure(code: "source_postcondition_failed")
            }
            report["typing_observed"] = true; report["source_verified"] = true
            phase = "验证真实截图"
            let screenshot = try await client.request("GET", "/screenshot?max_side=512&format=jpeg")
            guard let encoded = screenshot["value"] as? String, let bytes = Data(base64Encoded: encoded),
                  let picture = UIImage(data: bytes), picture.size.width > 0, picture.size.height > 0 else {
                throw PhoneRunnerClient.Failure(code: "screenshot_unavailable")
            }
            image = picture
            report["screenshot_sha256"] = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
            report["screenshot_width"] = picture.size.width; report["screenshot_height"] = picture.size.height
            report["ok"] = true
            phase = "点击、输入、读屏和截图通过"
        } catch {
            report["ok"] = false
            report["error_code"] = (error as? PhoneRunnerClient.Failure)?.code ?? "validation_request_failed"
            phase = report["error_code"] as? String ?? "验证失败"
        }
        report["finished_at"] = ISO8601DateFormatter().string(from: Date())
        report["elapsed_seconds"] = Date().timeIntervalSince(started)
        do {
            let data = try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            let url = documents.appendingPathComponent("self-host-control-smoke.json")
            try data.write(to: url, options: [.atomic, .completeFileProtection])
            reportURL = url
        } catch { phase = "验收报告保存失败" }
    }
}

struct ControlSmokeView: View {
    @StateObject private var model = ControlSmoke()
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button("验收点击") { model.count += 1 }.accessibilityIdentifier("selfhost-smoke-button")
                    Text("点击次数：\(model.count)")
                    TextField("输入验收文字", text: $model.text).accessibilityIdentifier("selfhost-smoke-text")
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                    Text(model.phase)
                    Button("执行独立控制验收") { Task { await model.run() } }.disabled(model.running)
                    Text("可以在电脑停止托管会话后运行。本页只操作自己的测试控件，截图显示在本机，报告不包含凭据或图片内容。")
                    if let url = model.reportURL { ShareLink("导出控制验收报告", item: url) }
                }
                if let image = model.image { Image(uiImage: image).resizable().scaledToFit() }
            }.navigationTitle("独立控制验收")
                .toolbar { Button("返回") { dismiss() } }
        }
    }
}
