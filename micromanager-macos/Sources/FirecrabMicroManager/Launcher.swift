import Darwin
import Foundation
@preconcurrency import Virtualization

@MainActor
final class VirtualMachineStopObserver: NSObject, @preconcurrency VZVirtualMachineDelegate {
    private var result: Result<Void, Error>?
    private var continuation: CheckedContinuation<Void, Error>?

    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        finish(.success(()))
    }

    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: any Error) {
        finish(.failure(error))
    }

    func wait() async throws {
        if let result {
            return try result.get()
        }
        try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
        }
    }

    private func finish(_ result: Result<Void, Error>) {
        guard self.result == nil else { return }
        self.result = result
        guard let continuation else { return }
        self.continuation = nil
        continuation.resume(with: result)
    }
}

private final class SendableVirtualMachine: @unchecked Sendable {
    let value: VZVirtualMachine

    init(_ value: VZVirtualMachine) {
        self.value = value
    }
}

@MainActor
func runManagementVM(configuration: VZVirtualMachineConfiguration) async throws {
    let virtualMachine = VZVirtualMachine(configuration: configuration)
    let observer = VirtualMachineStopObserver()
    virtualMachine.delegate = observer

    let sendableVirtualMachine = SendableVirtualMachine(virtualMachine)
    signal(SIGINT, SIG_IGN)
    signal(SIGTERM, SIG_IGN)
    let interruptSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
    let terminateSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
    let requestStop: @Sendable () -> Void = {
        do {
            try sendableVirtualMachine.value.requestStop()
            FileHandle.standardError.write(
                Data("microManager: orderly guest shutdown requested\n".utf8)
            )
        } catch {
            FileHandle.standardError.write(
                Data("microManager: could not request guest shutdown: \(error)\n".utf8)
            )
        }
    }
    interruptSource.setEventHandler(handler: requestStop)
    terminateSource.setEventHandler(handler: requestStop)
    interruptSource.resume()
    terminateSource.resume()
    defer {
        interruptSource.cancel()
        terminateSource.cancel()
    }

    try await virtualMachine.start()
    FileHandle.standardError.write(
        Data("microManager: Debian management VM started; press Control-C for orderly shutdown\n".utf8)
    )
    try await observer.wait()
}
