import Foundation
import Darwin

// Main-runloop Bonjour discovery. Collect the full six-second window before selecting.
final class RPDiscovery: NSObject, NetServiceBrowserDelegate, NetServiceDelegate {
    struct Candidate {
        let service: String
        let host: String
        let addresses: [String]
        let port: Int
    }
    private let browser = NetServiceBrowser()
    private var services: [NetService] = []
    private var candidates: [Candidate] = []
    private var deadline: Timer?
    private var completion: (([Candidate]) -> Void)?
    private var logger: ((String) -> Void)?

    func start(log: @escaping (String) -> Void, completion: @escaping ([Candidate]) -> Void) {
        cancel()
        self.logger = log; self.completion = completion
        browser.delegate = self
        log("Bonjour: browse _remotepairing._tcp.local for 6 s; resolve every service")
        browser.searchForServices(ofType: "_remotepairing._tcp.", inDomain: "local.")
        deadline = Timer.scheduledTimer(withTimeInterval: 6, repeats: false) { [weak self] _ in
            guard let self else { return }
            let result = self.candidates; let callback = self.completion
            self.cancel()
            callback?(result)
        }
    }
    func cancel() {
        deadline?.invalidate(); deadline = nil
        browser.stop()
        for service in services { service.stop(); service.delegate = nil }
        services.removeAll(); candidates.removeAll(); completion = nil; logger = nil
    }
    func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) {
        guard completion != nil else { return }
        logger?("Bonjour found: \(service.name).\(service.type)\(service.domain)")
        services.append(service); service.delegate = self; service.resolve(withTimeout: 5)
    }
    func netServiceBrowser(_ browser: NetServiceBrowser, didNotSearch errorDict: [String: NSNumber]) {
        logger?("Bonjour browse error: \(errorDict); wait until discovery deadline")
    }
    func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) {
        logger?("Bonjour resolve failed: \(sender.name), \(errorDict)")
    }
    func netServiceDidResolveAddress(_ sender: NetService) {
        let addresses = (sender.addresses ?? []).compactMap { data -> String? in
            data.withUnsafeBytes { bytes in
                guard let base = bytes.baseAddress, bytes.count >= MemoryLayout<sockaddr>.size else { return nil }
                let addr = base.assumingMemoryBound(to: sockaddr.self)
                let required: Int
                switch Int32(addr.pointee.sa_family) {
                case AF_INET: required = MemoryLayout<sockaddr_in>.size
                case AF_INET6: required = MemoryLayout<sockaddr_in6>.size
                default: return nil
                }
                guard bytes.count >= required else { return nil }
                var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                guard getnameinfo(addr, socklen_t(required), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 else { return nil }
                return String(cString: host)
            }
        }
        logger?("Bonjour resolved: \(sender.name), host=\(sender.hostName ?? "?"), addresses=\(addresses), port=\(sender.port)")
        candidates.append(Candidate(service: "\(sender.name).\(sender.type)\(sender.domain)", host: sender.hostName ?? "", addresses: addresses, port: sender.port))
    }
    // Normalize DNS spelling and numeric IPs (getnameinfo includes IPv6 scope,
    // while the C interface JSON does not). Compare binary IPs, not textual IPv6 forms.
    static func normalizedHost(_ host: String) -> String {
        var value = host.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "."))
        if value.hasSuffix(".local") { value.removeLast(6) }
        return value
    }
    static var localHost: String {
        var bytes = [CChar](repeating: 0, count: 256)
        guard gethostname(&bytes, bytes.count) == 0 else { return "" }
        return String(cString: bytes)
    }
    static func addressKey(_ address: String) -> String {
        let value = String(address.split(separator: "%").first ?? "")
        var v4 = in_addr(); var v6 = in6_addr()
        if value.withCString({ inet_pton(AF_INET, $0, &v4) }) == 1 {
            return withUnsafeBytes(of: v4) { "v4:" + $0.map { String(format: "%02x", $0) }.joined() }
        }
        if value.withCString({ inet_pton(AF_INET6, $0, &v6) }) == 1 {
            return withUnsafeBytes(of: v6) { "v6:" + $0.map { String(format: "%02x", $0) }.joined() }
        }
        return "invalid:" + value
    }
    static func localReason(_ candidate: Candidate, addresses: [String], host: String) -> String? {
        guard (1...65535).contains(candidate.port) else { return nil }
        var reasons: [String] = []
        if !host.isEmpty && normalizedHost(candidate.host) == normalizedHost(host) {
            reasons.append("host \(candidate.host) matches local hostname \(host)")
        }
        let localKeys = Set(addresses.map(addressKey))
        let matches = candidate.addresses.filter { localKeys.contains(addressKey($0)) }
        if !matches.isEmpty { reasons.append("local interface address match \(matches)") }
        return reasons.isEmpty ? nil : reasons.joined(separator: "; ")
    }
    static func select(_ candidates: [Candidate], addresses: [String], host: String) -> Candidate? {
        candidates.last { localReason($0, addresses: addresses, host: host) != nil }
    }
}
