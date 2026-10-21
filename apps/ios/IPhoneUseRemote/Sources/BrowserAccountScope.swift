import Foundation

extension BrowserAccount {
    /// A reused import ID does not authorize carrying the former website's
    /// session into a newly selected origin.
    mutating func updateScope(_ newDomains: [String]) {
        guard domains != newDomains else { return }
        let previousOrigin = domains.first
        domains = newDomains
        cookies = cookies.filter { $0.cookie(allowedDomains: newDomains) != nil }
        if previousOrigin != newDomains.first { localStorage = [:] }
        if let lastURL {
            if let url = URL(string: lastURL), url.scheme?.lowercased() == "https",
               let host = url.host, BrowserLibrary.domainAllowed(host, in: newDomains),
               url.user == nil, url.password == nil {
                // Keep the last page only when it belongs to the new scope.
            } else { self.lastURL = nil }
        }
        // Named WebKit stores must also drop former site data, even when the
        // user imported only bookmarks or personal fields this time.
        needsApply = true
    }
}
