import Foundation

enum LocalPacketReflection {
    static let client: [UInt8] = [10, 7, 1, 1]
    static let device: [UInt8] = [10, 7, 0, 1]
    static func reflect(_ packet: Data) -> Data? {
        var bytes = [UInt8](packet)
        guard bytes.count >= 20, bytes[0] >> 4 == 4 else { return nil }
        let headerLength = Int(bytes[0] & 15) * 4
        let totalLength = Int(bytes[2]) << 8 | Int(bytes[3])
        guard headerLength >= 20, totalLength >= headerLength, bytes.count >= totalLength else { return nil }
        let source = Array(bytes[12..<16]), destination = Array(bytes[16..<20])
        guard (source == client && destination == device) || (source == device && destination == client) else { return nil }
        bytes.replaceSubrange(12..<16, with: destination)
        bytes.replaceSubrange(16..<20, with: source)
        // Swapping address words preserves IPv4 and transport pseudo-header checksums.
        return Data(bytes.prefix(totalLength))
    }
}
