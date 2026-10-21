import Foundation
import Testing
@testable import FirecrabMicroManager

@Test
func managementNetworkIdentityIsStableAcrossBootsAndSeparateForEachHome() throws {
    let home = URL(fileURLWithPath: "/tmp/firecrab-network-one")
    let first = try ManagementVMConfiguration.makeNetwork(managedHomeURL: home)
    let next = try ManagementVMConfiguration.makeNetwork(managedHomeURL: home)
    let other = try ManagementVMConfiguration.makeNetwork(
        managedHomeURL: URL(fileURLWithPath: "/tmp/firecrab-network-two")
    )
    #expect(first.macAddress.string == next.macAddress.string)
    #expect(first.macAddress.string != other.macAddress.string)
    #expect(first.macAddress.isLocallyAdministeredAddress)
    #expect(first.macAddress.isUnicastAddress)
}

@Test
func managementNetworkCanPreserveAnExistingHostAddressMapping() throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let runtime = directory.appendingPathComponent("runtime")
    try FileManager.default.createDirectory(at: runtime, withIntermediateDirectories: true)
    let savedAddress = runtime.appendingPathComponent("network-mac-address")
    try "02:12:34:56:78:9a\n".write(to: savedAddress, atomically: true, encoding: .utf8)
    let network = try ManagementVMConfiguration.makeNetwork(managedHomeURL: directory)
    #expect(network.macAddress.string.lowercased() == "02:12:34:56:78:9a")
    try "not-a-mac\n".write(to: savedAddress, atomically: true, encoding: .utf8)
    #expect(throws: MicroManagerError.self) {
        try ManagementVMConfiguration.makeNetwork(managedHomeURL: directory)
    }
}

