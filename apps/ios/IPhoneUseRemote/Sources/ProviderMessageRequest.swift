import Foundation

/// A provider can disappear without invoking its completion handler. Finish
/// once, including cancellation before the continuation has been installed.
enum ProviderMessageOutcome: Sendable, Equatable {
    case response(Data?)
    case timedOut
    case cancelled
    case sendFailed
}

private final class ProviderMessageReply: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<ProviderMessageOutcome, Never>?
    private var outcome: ProviderMessageOutcome?

    func install(_ continuation: CheckedContinuation<ProviderMessageOutcome, Never>) -> Bool {
        lock.lock()
        if let outcome {
            lock.unlock()
            continuation.resume(returning: outcome)
            return false
        }
        self.continuation = continuation
        lock.unlock()
        return true
    }

    func finish(_ outcome: ProviderMessageOutcome) {
        lock.lock()
        guard self.outcome == nil else { lock.unlock(); return }
        self.outcome = outcome
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(returning: outcome)
    }
}

enum ProviderMessageRequest {
    @MainActor
    static func receive(timeoutNanoseconds: UInt64,
                        send: (@escaping @Sendable (Data?) -> Void) throws -> Void) async -> ProviderMessageOutcome {
        let reply = ProviderMessageReply()
        let deadline = Task.detached {
            do { try await Task.sleep(nanoseconds: timeoutNanoseconds) }
            catch { return }
            reply.finish(.timedOut)
        }
        defer { deadline.cancel() }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                guard reply.install(continuation) else { return }
                guard !Task.isCancelled else { reply.finish(.cancelled); return }
                do { try send { reply.finish(.response($0)) } }
                catch { reply.finish(.sendFailed) }
            }
        } onCancel: {
            reply.finish(.cancelled)
        }
    }
}
