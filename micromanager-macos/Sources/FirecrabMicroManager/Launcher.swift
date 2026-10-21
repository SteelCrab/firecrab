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

    func finish(_ result: Result<Void, Error>) {
        guard self.result == nil else { return }
        self.result = result
        guard let continuation else { return }
        self.continuation = nil
        continuation.resume(with: result)
    }
}

@MainActor
final class VirtualMachineShutdown {
    private let gracePeriod: Duration
    private let requestStop: () throws -> Void
    private let stop: () async throws -> Void
    private var task: Task<Void, Never>?

    init(
        gracePeriod: Duration = .seconds(20),
        requestStop: @escaping () throws -> Void,
        stop: @escaping () async throws -> Void
    ) {
        self.gracePeriod = gracePeriod
        self.requestStop = requestStop
        self.stop = stop
    }

    func request() {
        guard task == nil else { return }
        task = Task {
            do {
                try requestStop()
                FileHandle.standardError.write(
                    Data("microManager: orderly guest shutdown requested\n".utf8)
                )
            } catch {
                FileHandle.standardError.write(
                    Data("microManager: could not request guest shutdown: \(error)\n".utf8)
                )
            }
            do {
                try await Task.sleep(for: gracePeriod)
            } catch {
                return
            }
            // A panicked guest cannot acknowledge ACPI shutdown. Release its
            // VZ instance before launchd kills the supervising shell.
            do {
                try await stop()
            } catch {
                FileHandle.standardError.write(
                    Data("microManager: could not stop unresponsive VM: \(error)\n".utf8)
                )
            }
        }
    }

    func cancel() {
        task?.cancel()
    }
}

@MainActor
func runManagementVM(configuration: VZVirtualMachineConfiguration) async throws {
    let virtualMachine = VZVirtualMachine(configuration: configuration)
    let observer = VirtualMachineStopObserver()
    virtualMachine.delegate = observer

    let shutdown = VirtualMachineShutdown(
        requestStop: { try virtualMachine.requestStop() },
        stop: {
            if virtualMachine.state == .stopped {
                observer.finish(.success(()))
                return
            }
            guard virtualMachine.canStop else { return }
            FileHandle.standardError.write(
                Data("microManager: guest shutdown timed out; stopping VM\n".utf8)
            )
            try await virtualMachine.stop()
            observer.finish(.success(()))
        }
    )
    signal(SIGINT, SIG_IGN)
    signal(SIGTERM, SIG_IGN)
    let interruptSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
    let terminateSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
    let requestStop: @Sendable () -> Void = {
        Task { @MainActor in
            shutdown.request()
        }
    }
    interruptSource.setEventHandler(handler: requestStop)
    terminateSource.setEventHandler(handler: requestStop)
    interruptSource.resume()
    terminateSource.resume()
    defer {
        shutdown.cancel()
        interruptSource.cancel()
        terminateSource.cancel()
    }

    try await virtualMachine.start()
    FileHandle.standardError.write(
        Data("microManager: Debian management VM started; press Control-C for orderly shutdown\n".utf8)
    )
    try await observer.wait()
}
