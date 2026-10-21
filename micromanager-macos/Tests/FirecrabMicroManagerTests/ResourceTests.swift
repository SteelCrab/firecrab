import Foundation
import Testing
@testable import FirecrabMicroManager

@Test
func savedAllocationConfiguresTheManagedVMAndKeepsDiskPaths() throws {
    let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: home) }
    try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    let allocation = ManagerAllocation(cpu: 2, memoryMib: 1024, diskGib: 32,
        networkMode: "nat", networkInterface: nil)
    try JSONEncoder().encode(allocation).write(to: home.appendingPathComponent("resources.json"))
    let options = try VMOptions.configured(environment: ["FIRECRAB_MICROMANAGER_HOME": home.path])
    #expect(options.cpuCount == 2)
    #expect(options.memorySizeMiB == 1024)
    #expect(options.dataDiskURL == home.appendingPathComponent("data/firecrab-data.raw"))
    #expect(options.osDiskURL == home.appendingPathComponent("system/debian-system.raw"))
    try Data("broken json".utf8).write(to: home.appendingPathComponent("resources.json"))
    #expect(throws: (any Error).self) { try VMOptions.configured(environment: ["FIRECRAB_MICROMANAGER_HOME": home.path]) }
}

@Test
func allocationRejectsHostOverflowAndUnsupportedNetworks() {
    #expect(throws: MicroManagerError.self) {
        try ManagerAllocation(cpu: Int.max, memoryMib: 1024, diskGib: 16,
            networkMode: "nat", networkInterface: nil).validate()
    }
    #expect(throws: MicroManagerError.self) {
        try ManagerAllocation(cpu: 2, memoryMib: UInt64.max, diskGib: 16,
            networkMode: "nat", networkInterface: nil).validate()
    }
    #expect(throws: MicroManagerError.self) {
        try ManagerAllocation(cpu: 2, memoryMib: 1024, diskGib: 16,
            networkMode: "mirrored", networkInterface: nil).validate()
    }
    #expect(throws: MicroManagerError.self) {
        try ManagerAllocation(cpu: 2, memoryMib: 1024, diskGib: 16,
            networkMode: "bridged", networkInterface: "missing-interface").validate()
    }
}
