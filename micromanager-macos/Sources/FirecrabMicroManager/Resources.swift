import Foundation
import Security
@preconcurrency import Virtualization

struct ManagerAllocation: Codable, Equatable {
    let cpu: Int
    let memoryMib: UInt64
    let diskGib: UInt64
    let networkMode: String
    let networkInterface: String?

    static func read(home: URL) throws -> ManagerAllocation? {
        let path = home.appendingPathComponent("resources.json")
        guard FileManager.default.fileExists(atPath: path.path) else { return nil }
        return try JSONDecoder().decode(ManagerAllocation.self, from: Data(contentsOf: path))
    }

    func validate() throws {
        guard cpu >= VZVirtualMachineConfiguration.minimumAllowedCPUCount,
            cpu <= VZVirtualMachineConfiguration.maximumAllowedCPUCount,
            cpu <= ProcessInfo.processInfo.activeProcessorCount else {
            throw MicroManagerError.argument("CPU exceeds the host or virtualization limit")
        }
        let (bytes, overflow) = memoryMib.multipliedReportingOverflow(by: 1024 * 1024)
        guard !overflow, bytes >= VZVirtualMachineConfiguration.minimumAllowedMemorySize,
            bytes <= VZVirtualMachineConfiguration.maximumAllowedMemorySize,
            bytes <= ProcessInfo.processInfo.physicalMemory else {
            throw MicroManagerError.argument("RAM exceeds the host or virtualization limit")
        }
        guard diskGib > 0, diskGib <= 65536 else {
            throw MicroManagerError.argument("invalid data disk capacity")
        }
        guard networkMode == "nat" || networkMode == "bridged" else {
            throw MicroManagerError.argument("macOS supports nat or bridged networking")
        }
        if networkMode == "bridged" {
            guard bridgedNetworkingAuthorized() else {
                throw MicroManagerError.unsupported("bridged networking requires an Apple-approved com.apple.vm.networking entitlement; use NAT with the standard helper")
            }
            guard let networkInterface,
                VZBridgedNetworkInterface.networkInterfaces.contains(where: { $0.identifier == networkInterface }) else {
                throw MicroManagerError.argument("the selected physical network interface is unavailable")
            }
        } else if networkInterface != nil {
            throw MicroManagerError.argument("NAT does not use a physical network interface")
        }
    }
}

extension VMOptions {
    static func configured(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser
    ) throws -> VMOptions {
        let options = managedDefaults(environment: environment, homeDirectory: homeDirectory)
        guard let allocation = try ManagerAllocation.read(home: options.managedHomeURL) else { return options }
        try allocation.validate()
        return VMOptions(managedHomeURL: options.managedHomeURL, kernelURL: options.kernelURL,
            initialRamdiskURL: options.initialRamdiskURL, osDiskURL: options.osDiskURL,
            dataDiskURL: options.dataDiskURL, cpuCount: allocation.cpu,
            memorySizeMiB: allocation.memoryMib, commandLine: options.commandLine)
    }

    func publishActiveResources() throws {
        let desired = try ManagerAllocation.read(home: managedHomeURL)
        let size = try FileManager.default.attributesOfItem(atPath: dataDiskURL.path)[.size] as? NSNumber
        let allocation = ManagerAllocation(cpu: cpuCount, memoryMib: memorySizeMiB,
            diskGib: (size?.uint64Value ?? 0) / (1024 * 1024 * 1024),
            networkMode: desired?.networkMode ?? "nat", networkInterface: desired?.networkInterface)
        let data = try JSONEncoder().encode(allocation)
        try data.write(to: managedHomeURL.appendingPathComponent("runtime/resources-active.json"), options: .atomic)
    }
}

func resourceHostReport(home: URL) throws -> Data {
    struct Report: Encodable {
        let cpu: Int
        let memoryMib: UInt64
        let diskAvailableGib: UInt64
        let networkInterfaces: [String]
        let networkModes: [String]
    }
    var existing = home
    while !FileManager.default.fileExists(atPath: existing.path), existing.path != "/" {
        existing.deleteLastPathComponent()
    }
    let capacity = try existing.resourceValues(forKeys: [.volumeAvailableCapacityKey]).volumeAvailableCapacity ?? 0
    return try JSONEncoder().encode(Report(cpu: ProcessInfo.processInfo.activeProcessorCount,
        memoryMib: ProcessInfo.processInfo.physicalMemory / (1024 * 1024),
        diskAvailableGib: UInt64(max(0, capacity)) / (1024 * 1024 * 1024),
        networkInterfaces: VZBridgedNetworkInterface.networkInterfaces.map(\.identifier),
        networkModes: bridgedNetworkingAuthorized() ? ["nat", "bridged"] : ["nat"]))
}

private func bridgedNetworkingAuthorized() -> Bool {
    guard let task = SecTaskCreateFromSelf(nil),
        let value = SecTaskCopyValueForEntitlement(task, "com.apple.vm.networking" as CFString, nil) as? NSNumber else { return false }
    return value.boolValue
}
