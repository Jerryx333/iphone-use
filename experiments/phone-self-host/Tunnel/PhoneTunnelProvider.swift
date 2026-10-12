import Foundation
import NetworkExtension

/// Local loopback only. Address-swap approach based on LocalDevVPN/StosVPN.
/// See THIRD-PARTY-NOTICES.txt for attribution and license.
final class PhoneTunnelProvider: NEPacketTunnelProvider {
    private let stateQueue = DispatchQueue(label: "ipu-phone-tunnel-state")
    private var results: [ResultRow] = []
    private var running = false
    private var bridge: ScopedTunnelBridge?
    private var hostError: String?
    private var monitor: DispatchSourceTimer?
    private var failures = 0
    private var retryAfter = Date.distantPast
    private var sessionRequested: Bool?
    private var stoppingHost = false
    private var packetCount = 0
    private var reflectedPacketCount = 0
    private var hostStopCompletions: [() -> Void] = []

    override func startTunnel(options: [String: NSObject]?, completionHandler: @escaping (Error?) -> Void) {
        let settings = NEPacketTunnelNetworkSettings(tunnelRemoteAddress: "10.7.0.1")
        let ipv4 = NEIPv4Settings(addresses: ["10.7.1.1"], subnetMasks: ["255.255.255.255"])
        ipv4.includedRoutes = [NEIPv4Route(destinationAddress: "10.7.0.1", subnetMask: "255.255.255.255")]
        ipv4.excludedRoutes = [.default()]
        settings.ipv4Settings = ipv4
        setTunnelNetworkSettings(settings) { error in
            guard error == nil else { completionHandler(error); return }
            self.stateQueue.sync { self.running = true; self.results = [] }
            self.reflectPackets()
            completionHandler(nil)
            self.runProbes()
            self.stateQueue.async {
                self.startMonitor()
                self.ensureBridge()
            }
        }
    }
    private func reflectPackets() {
        packetFlow.readPackets { packets, protocols in
            guard self.stateQueue.sync(execute: { self.running }) else { return }
            var reflected: [Data] = []
            var families: [NSNumber] = []
            for (packet, family) in zip(packets, protocols) {
                self.stateQueue.sync { self.packetCount += 1 }
                guard family.int32Value == AF_INET, let changed = LocalPacketReflection.reflect(packet) else { continue }
                self.stateQueue.sync { self.reflectedPacketCount += 1 }
                reflected.append(changed)
                families.append(family)
            }
            if !reflected.isEmpty { self.packetFlow.writePackets(reflected, withProtocols: families) }
            self.reflectPackets()
        }
    }
    override func stopTunnel(with reason: NEProviderStopReason, completionHandler: @escaping () -> Void) {
        stateQueue.async {
            self.running = false
            self.monitor?.cancel(); self.monitor = nil
            self.stopHost {
                self.bridge?.stop(); self.bridge = nil
                completionHandler()
            }
        }
    }
    private func runProbes() {
        Task {
            var collected = [ResultRow]()
            for host in ["127.0.0.1", "10.7.0.1"] {
                collected.append(await TCPAttempt(Endpoint(host: host, port: 49152, purpose: "extension direct RemotePairing")).run())
            }
            collected.append(await TunnelTCPAttempt(provider: self).run())
            collected.append(await TunnelTCPAttempt(provider: self, port: 8080).run())
            var interfaces: UnsafeMutablePointer<ifaddrs>?
            if getifaddrs(&interfaces) == 0, let first = interfaces {
                defer { freeifaddrs(interfaces) }
                var cursor: UnsafeMutablePointer<ifaddrs>? = first
                while let current = cursor {
                    defer { cursor = current.pointee.ifa_next }
                    guard String(cString: current.pointee.ifa_name) == "en0",
                          let address = current.pointee.ifa_addr,
                          address.pointee.sa_family == UInt8(AF_INET) else { continue }
                    var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                    if getnameinfo(address, socklen_t(address.pointee.sa_len), &host, socklen_t(host.count), nil, 0, NI_NUMERICHOST) == 0 {
                        collected.append(await TCPAttempt(Endpoint(host: String(cString: host), port: 49152, purpose: "extension direct own Wi-Fi RemotePairing")).run())
                    }
                }
            }
            self.stateQueue.sync { if self.running { self.results = collected } }
        }
    }
    private func ensureBridge() {
        guard running, bridge == nil else { return }
        do {
            let token = try ProtectedStore.token("bridge_token")
            let bridge = ScopedTunnelBridge(provider: self, token: token)
            self.bridge = bridge
            bridge.start { error in
                self.stateQueue.async {
                    guard self.bridge === bridge else { return }
                    if error != nil {
                        self.hostError = "bridge_start_failed"
                        bridge.stop(); self.bridge = nil
                        NativeHost.stop()
                    } else { self.hostError = nil }
                }
            }
        } catch { hostError = "keychain_unavailable" }
    }
    private func startMonitor() {
        let timer = DispatchSource.makeTimerSource(queue: stateQueue)
        timer.schedule(deadline: .now() + 2, repeating: 5)
        timer.setEventHandler { [weak self] in
            guard let self, self.running, !self.stoppingHost else { return }
            if self.bridge == nil { self.ensureBridge(); return }
            guard let bridge = self.bridge, let port = bridge.port else { return }
            let status = NativeHost.status()
            let phase = status["phase"] as? String ?? "unknown"
            if phase == "running" { self.failures = 0; return }
            let requested = self.sessionRequested ?? (ProtectedStore.read("session_enabled") == Data([1]))
            guard requested,
                  ProtectedStore.read("pair_record") != nil,
                  ["stopped", "runner_exited", "failed"].contains(phase), Date() >= self.retryAfter else { return }
            let error = status["error_code"] as? String
            // Pairing and authorization failures need a person; retry transport loss only.
            if let error, !["bridge_connect_failed", "bridge_write_failed", "bridge_ack_failed", "device_connect_failed", "tunnel_timeout",
                "tunnel_listener_failed", "tunnel_handshake_failed", "rsd_connect_failed",
                "rsd_handshake_failed", "xctest_failed"].contains(error) { return }
            self.failures += 1
            self.retryAfter = Date().addingTimeInterval(min(300, 15 * pow(2, Double(min(self.failures, 5)))))
            self.hostError = NativeHost.start(mode: "launch", bridgePort: port, bridgeToken: bridge.token)
        }
        monitor = timer
        timer.resume()
    }
    override func handleAppMessage(_ messageData: Data, completionHandler: ((Data?) -> Void)? = nil) {
        stateQueue.async { self.respondToMessage(messageData, completionHandler: completionHandler) }
    }
    // Commands, recovery ticks and tunnel shutdown share one executor. A stop
    // cannot interleave with a newly accepted launch's Keychain token write.
    private func respondToMessage(_ messageData: Data, completionHandler: ((Data?) -> Void)?) {
        let command = String(data: messageData, encoding: .utf8)
        if ["pair", "probe", "launch"].contains(command ?? "") {
            guard !stoppingHost else {
                completionHandler?(try? JSONSerialization.data(withJSONObject: ["error_code": "independent_stop_in_progress"]))
                return
            }
            if command == "launch" {
                do {
                    try ProtectedStore.write("session_enabled", Data([1]))
                    sessionRequested = true
                } catch { hostError = "keychain_unavailable"; completionHandler?(nil); return }
            }
            if let bridge, let port = bridge.port {
                let error = NativeHost.start(mode: command!, bridgePort: port, bridgeToken: bridge.token)
                hostError = error
            } else { hostError = "bridge_not_ready" }
        } else if command == "restart" {
            guard !stoppingHost else {
                completionHandler?(try? JSONSerialization.data(withJSONObject: ["error_code": "independent_stop_in_progress"]))
                return
            }
            do {
                try ProtectedStore.write("session_enabled", Data([1]))
                // Keep automatic recovery out until the old process has
                // released its control ports. The persisted intent stays on.
                sessionRequested = false
            } catch { hostError = "keychain_unavailable"; completionHandler?(nil); return }
            // The main app exits with XCTest. Finish and relaunch inside the
            // extension, which stays alive without any Mac supervisor.
            stopHost {
                self.stateQueue.asyncAfter(deadline: .now() + 1) {
                    self.respondToMessage(Data("launch".utf8), completionHandler: completionHandler)
                }
            }
            return
        } else if command == "stop-host" {
            sessionRequested = false
            do { try ProtectedStore.write("session_enabled", Data([0])) }
            catch { hostError = "keychain_unavailable" }
            stopHost {
                self.respondToMessage(Data("status".utf8), completionHandler: completionHandler)
            }
            return
        }
        else if command != "status" { completionHandler?(nil); return }
        let (rows, error) = (results, hostError)
        var report: [String: Any] = ["native": NativeHost.status()]
        report["packet_count"] = packetCount
        report["reflected_packet_count"] = reflectedPacketCount
        if let data = try? JSONEncoder().encode(rows), let value = try? JSONSerialization.jsonObject(with: data) {
            report["probes"] = value
        }
        if let error { report["error_code"] = error }
        completionHandler?(try? JSONSerialization.data(withJSONObject: report))
    }
    private func stopHost(completion: @escaping () -> Void) {
        hostStopCompletions.append(completion)
        guard !stoppingHost else { return }
        stoppingHost = true
        NativeHost.stopGracefully {
            self.stateQueue.async {
                self.stoppingHost = false
                let callbacks = self.hostStopCompletions
                self.hostStopCompletions.removeAll()
                callbacks.forEach { $0() }
            }
        }
    }
}

/// A Network Extension's normal sockets bypass its own tunnel. This API opts in.
final class TunnelTCPAttempt {
    private let provider: NEPacketTunnelProvider
    private let queue = DispatchQueue(label: "ipu-through-tunnel-probe")
    private var connection: NWTCPConnection?
    private var observation: NSKeyValueObservation?
    private var continuation: CheckedContinuation<ResultRow, Never>?
    private var finished = false
    private var sent = false
    private var bytes = Data()
    private let started = Date()
    private let endpoint: Endpoint
    init(provider: NEPacketTunnelProvider, port: UInt16 = 49152) {
        self.provider = provider
        self.endpoint = Endpoint(host: "10.7.0.1", port: port, purpose: "extension through-tunnel RemotePairing")
    }
    func run() async -> ResultRow {
        await withCheckedContinuation { continuation in
            queue.async {
                self.continuation = continuation
                let endpoint = NWHostEndpoint(hostname: "10.7.0.1", port: String(self.endpoint.port))
                let connection = self.provider.createTCPConnectionThroughTunnel(to: endpoint, enableTLS: false, tlsParameters: nil, delegate: nil)
                self.connection = connection
                self.observation = connection.observe(\.state, options: [.new, .initial]) { conn, _ in
                    self.queue.async {
                        guard !self.finished else { return }
                        switch conn.state {
                        case .connected:
                            guard !self.sent else { return }
                            self.sent = true
                            do {
                                let request = try RemotePairingHandshake.request()
                                conn.write(request) { error in
                                    self.queue.async {
                                        if let error { self.finish("handshake_send_failed", error.localizedDescription) }
                                        else { self.receive() }
                                    }
                                }
                            } catch { self.finish("invalid_request", "Failed to encode handshake") }
                        case .disconnected, .cancelled:
                            self.finish("tunnel_connection_closed", "Connection closed before handshake")
                        default: break
                        }
                    }
                }
                self.queue.asyncAfter(deadline: .now() + 8) { self.finish("probe_timeout", "No handshake within 8 seconds") }
            }
        }
    }
    private func receive() {
        connection?.readMinimumLength(1, maximumLength: 65546) { data, error in
            self.queue.async {
                guard !self.finished else { return }
                if let error { self.finish("handshake_receive_failed", error.localizedDescription); return }
                guard let data, !data.isEmpty else { self.finish("handshake_closed", "Peer closed"); return }
                self.bytes.append(data)
                do {
                    if let summary = try RemotePairingHandshake.response(self.bytes) { self.finish("remote_pairing_handshake", summary) }
                    else { self.receive() }
                } catch { self.finish("handshake_unrecognized", "Response did not match protocol") }
            }
        }
    }
    private func finish(_ outcome: String, _ detail: String) {
        guard !finished else { return }
        finished = true
        observation?.invalidate(); observation = nil
        connection?.cancel(); connection = nil
        continuation?.resume(returning: ResultRow(endpoint: endpoint, outcome: outcome, detail: detail,
            elapsedMilliseconds: Int(Date().timeIntervalSince(started) * 1000)))
        continuation = nil
    }
}
