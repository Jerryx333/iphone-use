import Foundation

struct CatalogModel: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let readsImages: Bool
}

/// Lists the service's current IDs; a successful listing is not a successful inference.
@MainActor
enum ModelCatalog {
    static func parse(_ data: Data, kind: ModelProvider.Kind) throws -> [CatalogModel] {
        let json = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        let rows = json?["data"] as? [[String: Any]] ?? []
        var seen = Set<String>()
        return rows.compactMap { row -> CatalogModel? in
            guard let id = row["id"] as? String, !id.isEmpty,
                  row["deprecated"] as? Bool != true,
                  !["deprecated", "retired", "disabled"].contains(row["status"] as? String ?? ""),
                  seen.insert(id).inserted else { return nil }
            let lower = id.lowercased()
            guard !["guard", "moderation", "embedding", "safety"].contains(where: lower.contains) else { return nil }
            if kind == .free && !id.hasSuffix(":free") { return nil }
            if kind == .zen {
                // Zen's listing has no protocol metadata. These use SystemOne or
                // Responses, which are not supported by our chat task contract.
                guard (id.hasSuffix("-free") || id == "big-pickle"),
                      !id.hasPrefix("jev-"), !id.hasPrefix("muse-spark-") else { return nil }
            }
            if let parameters = row["supported_parameters"] as? [String], !parameters.contains("tools") { return nil }
            let architecture = row["architecture"] as? [String: Any]
            let modalities = architecture?["input_modalities"] as? [String] ?? []
            return CatalogModel(id: id, name: row["name"] as? String ?? id, readsImages: modalities.contains("image"))
        }.sorted {
            if $0.readsImages != $1.readsImages { return $0.readsImages }
            return $0.name.localizedStandardCompare($1.name) == .orderedAscending
        }
    }

    static func fetch(_ provider: ModelProvider, session suppliedSession: URLSession? = nil) async throws -> [CatalogModel] {
        let base = provider.baseURL.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let url = URL(string: base + "/models"), let host = url.host,
              url.scheme == "https" || (url.scheme == "http" && ["localhost", "127.0.0.1", "::1"].contains(host)) else {
            throw BrowserToolError.actionFailed(String(localized: "模型服务地址需要使用 HTTPS"))
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        let key = provider.requestKey(provider.needsKey ? ModelSettingsStore.secret(provider.keyAccount) : nil)
        if provider.format == .anthropic {
            request.setValue(key, forHTTPHeaderField: "x-api-key")
            request.setValue("2023-06-01", forHTTPHeaderField: "anthropic-version")
        } else if !key.isEmpty {
            request.setValue("Bearer " + key, forHTTPHeaderField: "Authorization")
        }
        let session = suppliedSession ?? URLSession(configuration: .ephemeral, delegate: BrowserModelRedirectGuard(), delegateQueue: nil)
        defer { session.invalidateAndCancel() }
        let (data, response) = try await session.data(for: request)
        try validateModelResponse(data, response: response)
        let models = try parse(data, kind: provider.kind)
        guard !models.isEmpty else {
            throw BrowserToolError.actionFailed(String(localized: "服务没有返回可用的模型，可以手动填写模型 ID"))
        }
        return models
    }

    static func check(_ provider: ModelProvider) async throws {
        let key = provider.requestKey(provider.needsKey ? ModelSettingsStore.secret(provider.keyAccount) : nil)
        if provider.needsKey && key.isEmpty {
            throw BrowserToolError.actionFailed(String(localized: "请先填写密钥"))
        }
        _ = try await BrowserModelClient().decide(
            goal: "Connection test with synthetic data only. Call browser_action with action done and explanation 连接正常.",
            page: nil, steps: [], models: [.init(format: provider.format.rawValue, baseURL: provider.baseURL, key: key, model: provider.model)])
    }
}

func validateModelResponse(_ data: Data, response: URLResponse) throws {
    guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
        let code = (response as? HTTPURLResponse)?.statusCode ?? 0
        let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        let error = object?["error"] as? [String: Any]
        if code == 403, error?["type"] as? String == "FreeTierError" {
            throw BrowserToolError.actionFailed(String(localized: "Zen 免费层限制了第三方客户端（403）。请选择 Kilo 免费模型或其他服务。"))
        }
        throw BrowserToolError.actionFailed(String(localized: "模型服务请求失败（\(code)），请检查模型设置后重试"))
    }
}
