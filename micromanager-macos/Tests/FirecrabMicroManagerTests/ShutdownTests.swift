import Foundation
import Testing
@testable import FirecrabMicroManager

@MainActor
@Test
func unresponsiveGuestShutdownStopsOnceAfterGracePeriod() async throws {
    var requests = 0
    var stops = 0
    let shutdown = VirtualMachineShutdown(
        gracePeriod: .milliseconds(5),
        requestStop: { requests += 1 },
        stop: { stops += 1 }
    )
    defer { shutdown.cancel() }
    shutdown.request()
    shutdown.request()
    for _ in 0..<100 where stops == 0 {
        try await Task.sleep(for: .milliseconds(5))
    }
    #expect(requests == 1)
    #expect(stops == 1)
}

@MainActor
@Test
func failedShutdownRequestStillStopsUnresponsiveGuest() async throws {
    var stops = 0
    let shutdown = VirtualMachineShutdown(
        gracePeriod: .milliseconds(5),
        requestStop: { throw NSError(domain: "shutdown-test", code: 1) },
        stop: { stops += 1 }
    )
    defer { shutdown.cancel() }
    shutdown.request()
    for _ in 0..<100 where stops == 0 {
        try await Task.sleep(for: .milliseconds(5))
    }
    #expect(stops == 1)
}

@MainActor
@Test
func gracefulShutdownCancelsFallbackStop() async throws {
    var stops = 0
    let shutdown = VirtualMachineShutdown(
        gracePeriod: .milliseconds(10),
        requestStop: {},
        stop: { stops += 1 }
    )
    shutdown.request()
    shutdown.cancel()
    try await Task.sleep(for: .milliseconds(30))
    #expect(stops == 0)
}

