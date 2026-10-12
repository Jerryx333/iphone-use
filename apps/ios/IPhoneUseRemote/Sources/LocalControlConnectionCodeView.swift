import CoreImage.CIFilterBuiltins
import Darwin
import SwiftUI
import UIKit

@MainActor
struct LocalControlConnectionCodeView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var image: UIImage?
    @State private var error: String?

    var body: some View {
        AppNavigationStack {
            VStack(spacing: 24) {
                if let image { Image(uiImage: image).interpolation(.none).resizable().scaledToFit().frame(maxWidth: 280) }
                else if error == nil { ProgressView("正在生成连接码") }
                Text("用另一台设备上的 Phone Use 扫码连接。只向你信任的设备展示；连接码一次有效，5 分钟后过期。")
                if let error { Text(error).foregroundStyle(.secondary) }
            }.padding().navigationTitle("连接这台 iPhone")
                .toolbar { Button("完成") { dismiss() } }
        }.task { await load() }
    }

    private func load() async {
        do {
            guard let address = Self.wifiAddress() else { error = "请先连接 Wi-Fi。"; return }
            let token = try ProtectedStore.token("runner_token")
            let target = "/daemon/pair-code"
            var request = URLRequest(url: URL(string: "http://127.0.0.1:18100" + target)!)
            request.httpMethod = "POST"
            request.httpBody = Data()
            request.timeoutInterval = 5
            request.setValue(RunnerSigning.authorization(token: token, method: "POST", target: target,
                timestamp: Int64(Date().timeIntervalSince1970), nonce: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(), body: Data()), forHTTPHeaderField: "Authorization")
            let (data, response) = try await URLSession.shared.data(for: request)
            guard (response as? HTTPURLResponse)?.statusCode == 200, data.count < 4096,
                  let json = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let value = json["value"] as? [String: Any], let code = value["code"] as? String,
                  !code.isEmpty, code.count <= 128, let port = value["port"] as? Int, port == 18787 else {
                error = "本机控制尚未就绪，请稍后重试。"; return
            }
            var url = URLComponents(string: "http://\(address):\(port)/pair")!
            url.queryItems = [URLQueryItem(name: "c", value: code)]
            guard let text = url.url?.absoluteString else { return }
            let filter = CIFilter.qrCodeGenerator()
            filter.message = Data(text.utf8)
            filter.correctionLevel = "M"
            guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 8, y: 8)),
                  let cg = CIContext().createCGImage(output, from: output.extent) else { return }
            image = UIImage(cgImage: cg)
        } catch { self.error = "暂时无法生成连接码，请确认本机控制已启用。" }
    }

    private static func wifiAddress() -> String? {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return nil }
        defer { freeifaddrs(list) }
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let current = cursor {
            defer { cursor = current.pointee.ifa_next }
            guard String(cString: current.pointee.ifa_name) == "en0", let address = current.pointee.ifa_addr,
                  address.pointee.sa_family == UInt8(AF_INET) else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            if getnameinfo(address, socklen_t(address.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                return String(cString: host)
            }
        }
        return nil
    }
}
