import Foundation
import WebKit

/// Imports the selected HTTPS origin's initial storage, then removes only
/// this bootstrap script so later navigation cannot resurrect stale logins.
@MainActor
final class BrowserStorageRestore {
    private weak var controller: WKUserContentController?
    private let host: String
    private var script: WKUserScript?

    init(controller: WKUserContentController, host: String, values: [String: JSONValue]) throws {
        self.controller = controller
        self.host = host
        let data = try JSONEncoder().encode(values)
        let originData = try JSONEncoder().encode("https://" + host)
        guard let json = String(data: data, encoding: .utf8),
              let origin = String(data: originData, encoding: .utf8) else { throw URLError(.cannotParseResponse) }
        let source = "if (location.origin === " + origin + ") { const values = " + json + "; for (const [key,value] of Object.entries(values)) { if (!['__proto__','constructor','prototype'].includes(key)) localStorage.setItem(key, typeof value === 'string' ? value : JSON.stringify(value)); } }"
        let script = WKUserScript(source: source, injectionTime: .atDocumentStart, forMainFrameOnly: true)
        self.script = script
        controller.addUserScript(script)
    }

    func completedNavigation(to url: URL) {
        guard url.scheme?.lowercased() == "https", url.host?.lowercased() == host,
              url.port == nil || url.port == 443,
              let script, let controller else { return }
        let others = controller.userScripts.filter { $0 !== script }
        controller.removeAllUserScripts()
        for other in others { controller.addUserScript(other) }
        self.script = nil
    }
}
