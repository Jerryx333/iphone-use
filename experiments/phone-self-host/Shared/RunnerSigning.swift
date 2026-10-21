import Foundation
import CryptoKit

enum RunnerSigning {
    static func authorization(token: String, method: String, target: String, timestamp: Int64, nonce: String, body: Data) -> String {
        let digest = SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined()
        let input = "ipu-runner-v1\n\(method.uppercased())\n\(target)\n\(timestamp)\n\(nonce)\n\(digest)"
        let key = SymmetricKey(data: Data(token.utf8))
        let signature = HMAC<SHA256>.authenticationCode(for: Data(input.utf8), using: key)
            .map { String(format: "%02x", $0) }.joined()
        return "IPU-HMAC-SHA256 ts=\(timestamp), nonce=\(nonce), sig=\(signature)"
    }
}
