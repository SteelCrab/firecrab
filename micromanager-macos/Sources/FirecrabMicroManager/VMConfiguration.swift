import Foundation
@preconcurrency import Virtualization

enum MicroManagerError: Error, CustomStringConvertible, Equatable {
    case argument(String)
    case unsupported(String)
    case invalidArtifact(String)

    var description: String {
        switch self {
        case let .argument(message), let .unsupported(message), let .invalidArtifact(message):
            return message
        }
    }
}

struct VMOptions: Equatable, Sendable {
    let managedHomeURL: URL
    let kernelURL: URL
    let initialRamdiskURL: URL?
    let osDiskURL: URL
    let dataDiskURL: URL
    let cpuCount: Int
    let memorySizeMiB: UInt64
    let commandLine: String

    static let defaultCommandLine =
        "console=hvc0 root=/dev/vda1 ro systemd.unified_cgroup_hierarchy=1"

    private static func commandLine(managedHomeURL: URL) -> String {
        let metadataURL = managedHomeURL.appendingPathComponent("runtime/root-partuuid")
        guard
            let value = try? String(contentsOf: metadataURL, encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines),
            let partuuid = UUID(uuidString: value)
        else {
            return defaultCommandLine
        }
        return "console=hvc0 root=PARTUUID=\(partuuid.uuidString.lowercased()) ro systemd.unified_cgroup_hierarchy=1"
    }

    static func managedDefaults(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        homeDirectory: URL = FileManager.default.homeDirectoryForCurrentUser
    ) -> VMOptions {
        let managedHomeURL: URL
        if let override = environment["FIRECRAB_MICROMANAGER_HOME"], !override.isEmpty {
            managedHomeURL = fileURL(override)
        } else {
            managedHomeURL = homeDirectory
                .appendingPathComponent("Library/Application Support/Firecrab/micromanager")
                .standardizedFileURL
        }
        let systemDirectory = managedHomeURL.appendingPathComponent("system", isDirectory: true)
        let dataDirectory = managedHomeURL.appendingPathComponent("data", isDirectory: true)
        return VMOptions(
            managedHomeURL: managedHomeURL,
            kernelURL: systemDirectory.appendingPathComponent("Image"),
            initialRamdiskURL: systemDirectory.appendingPathComponent("initrd.img"),
            osDiskURL: systemDirectory.appendingPathComponent("debian-system.raw"),
            dataDiskURL: dataDirectory.appendingPathComponent("firecrab-data.raw"),
            cpuCount: 2,
            memorySizeMiB: 4096,
            commandLine: commandLine(managedHomeURL: managedHomeURL)
        )
    }
}

private func fileURL(_ path: String) -> URL {
    URL(fileURLWithPath: path, relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath))
        .standardizedFileURL
        .resolvingSymlinksInPath()
}

enum ManagementVMConfiguration {
    static func makeProvisioning(
        options: VMOptions,
        consoleInput: FileHandle = .standardInput,
        consoleOutput: FileHandle = .standardOutput
    ) throws -> VZVirtualMachineConfiguration {
        guard VZVirtualMachine.isSupported else {
            throw MicroManagerError.unsupported(
                "Virtualization.framework is unavailable; run `firecrab service doctor`."
            )
        }
        guard VZGenericPlatformConfiguration.isNestedVirtualizationSupported else {
            throw MicroManagerError.unsupported(
                "nested virtualization is unavailable; an M3-or-later supported host is required"
            )
        }

        let seedURL = options.managedHomeURL
            .appendingPathComponent("runtime/cloud-init-seed.iso")
        let variableStoreURL = options.managedHomeURL
            .appendingPathComponent("runtime/efi-variable-store")
        try validateRegularFile(options.osDiskURL, label: "OS disk", writable: true)
        try validateRegularFile(options.dataDiskURL, label: "data disk", writable: true)
        try validateRegularFile(seedURL, label: "cloud-init seed", writable: false)
        try validateSeparateDisks(options.osDiskURL, options.dataDiskURL)
        let memorySize = try validatedMemorySize(options)

        let platform = VZGenericPlatformConfiguration()
        platform.isNestedVirtualizationEnabled = true

        let bootLoader = VZEFIBootLoader()
        if FileManager.default.fileExists(atPath: variableStoreURL.path) {
            bootLoader.variableStore = VZEFIVariableStore(url: variableStoreURL)
        } else {
            bootLoader.variableStore = try VZEFIVariableStore(
                creatingVariableStoreAt: variableStoreURL
            )
        }

        let osAttachment = try VZDiskImageStorageDeviceAttachment(
            url: options.osDiskURL,
            readOnly: false
        )
        let dataAttachment = try VZDiskImageStorageDeviceAttachment(
            url: options.dataDiskURL,
            readOnly: false
        )
        let seedAttachment = try VZDiskImageStorageDeviceAttachment(
            url: seedURL,
            readOnly: true
        )

        let network = VZVirtioNetworkDeviceConfiguration()
        network.attachment = VZNATNetworkDeviceAttachment()

        let console = VZVirtioConsoleDeviceSerialPortConfiguration()
        console.attachment = VZFileHandleSerialPortAttachment(
            fileHandleForReading: consoleInput,
            fileHandleForWriting: consoleOutput
        )

        let sharedDirectory = VZSharedDirectory(
            url: options.managedHomeURL,
            readOnly: false
        )
        let fileSystem = VZVirtioFileSystemDeviceConfiguration(tag: "firecrab")
        fileSystem.share = VZSingleDirectoryShare(directory: sharedDirectory)

        let configuration = VZVirtualMachineConfiguration()
        configuration.platform = platform
        configuration.bootLoader = bootLoader
        configuration.cpuCount = options.cpuCount
        configuration.memorySize = memorySize
        configuration.storageDevices = [
            VZVirtioBlockDeviceConfiguration(attachment: osAttachment),
            VZVirtioBlockDeviceConfiguration(attachment: dataAttachment),
            VZVirtioBlockDeviceConfiguration(attachment: seedAttachment),
        ]
        configuration.networkDevices = [network]
        configuration.serialPorts = [console]
        configuration.directorySharingDevices = [fileSystem]
        configuration.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        configuration.memoryBalloonDevices = [
            VZVirtioTraditionalMemoryBalloonDeviceConfiguration(),
        ]
        configuration.socketDevices = [VZVirtioSocketDeviceConfiguration()]
        try configuration.validate()
        return configuration
    }

    static func inspect(options: VMOptions) -> [Diagnostic] {
        var checks = [
            configurationDiagnostic(
                id: "managed_home",
                detail: options.managedHomeURL.path
            ) {
                try validateDirectory(options.managedHomeURL, label: "managed home")
            },
            configurationDiagnostic(id: "kernel", detail: options.kernelURL.path) {
                try validateRegularFile(options.kernelURL, label: "kernel", writable: false)
                try validateArm64Kernel(options.kernelURL)
            },
        ]
        if let initialRamdiskURL = options.initialRamdiskURL {
            checks.append(
                configurationDiagnostic(id: "initrd", detail: initialRamdiskURL.path) {
                    try validateRegularFile(initialRamdiskURL, label: "initrd", writable: false)
                }
            )
        } else {
            checks.append(
                Diagnostic(
                    id: "initrd",
                    status: .fail,
                    detail: "initial RAM disk is not configured",
                    fix: nil
                )
            )
        }
        checks.append(
            configurationDiagnostic(id: "os_disk", detail: options.osDiskURL.path) {
                try validateRegularFile(options.osDiskURL, label: "OS disk", writable: true)
            }
        )
        checks.append(
            configurationDiagnostic(id: "data_disk", detail: options.dataDiskURL.path) {
                try validateRegularFile(options.dataDiskURL, label: "data disk", writable: true)
            }
        )
        checks.append(
            configurationDiagnostic(
                id: "disk_separation",
                detail: "OS and persistent data disks are separate"
            ) {
                try validateSeparateDisks(options.osDiskURL, options.dataDiskURL)
            }
        )
        checks.append(
            configurationDiagnostic(
                id: "resources",
                detail: "\(options.cpuCount) CPUs, \(options.memorySizeMiB) MiB memory"
            ) {
                _ = try validatedMemorySize(options)
            }
        )

        if !checks.contains(where: { $0.status == .fail }) {
            checks.append(
                configurationDiagnostic(
                    id: "virtual_machine",
                    detail: "Apple Virtualization.framework configuration is valid"
                ) {
                    _ = try make(options: options)
                }
            )
        }
        return checks
    }

    static func make(
        options: VMOptions,
        consoleInput: FileHandle = .standardInput,
        consoleOutput: FileHandle = .standardOutput
    ) throws -> VZVirtualMachineConfiguration {
        guard VZVirtualMachine.isSupported else {
            throw MicroManagerError.unsupported(
                "Virtualization.framework is unavailable; run `firecrab service doctor`."
            )
        }
        guard VZGenericPlatformConfiguration.isNestedVirtualizationSupported else {
            throw MicroManagerError.unsupported(
                "nested virtualization is unavailable; an M3-or-later supported host is required"
            )
        }

        try validateRegularFile(options.kernelURL, label: "kernel", writable: false)
        try validateArm64Kernel(options.kernelURL)
        if let initialRamdiskURL = options.initialRamdiskURL {
            try validateRegularFile(initialRamdiskURL, label: "initrd", writable: false)
        }
        try validateRegularFile(options.osDiskURL, label: "OS disk", writable: true)
        try validateRegularFile(options.dataDiskURL, label: "data disk", writable: true)
        try validateSeparateDisks(options.osDiskURL, options.dataDiskURL)

        let memorySize = try validatedMemorySize(options)

        let platform = VZGenericPlatformConfiguration()
        platform.isNestedVirtualizationEnabled = true

        let bootLoader = VZLinuxBootLoader(kernelURL: options.kernelURL)
        bootLoader.initialRamdiskURL = options.initialRamdiskURL
        bootLoader.commandLine = options.commandLine

        let osAttachment = try VZDiskImageStorageDeviceAttachment(
            url: options.osDiskURL,
            readOnly: false
        )
        let dataAttachment = try VZDiskImageStorageDeviceAttachment(
            url: options.dataDiskURL,
            readOnly: false
        )

        let network = VZVirtioNetworkDeviceConfiguration()
        network.attachment = VZNATNetworkDeviceAttachment()

        let console = VZVirtioConsoleDeviceSerialPortConfiguration()
        console.attachment = VZFileHandleSerialPortAttachment(
            fileHandleForReading: consoleInput,
            fileHandleForWriting: consoleOutput
        )

        let sharedDirectory = VZSharedDirectory(
            url: options.managedHomeURL,
            readOnly: false
        )
        let fileSystem = VZVirtioFileSystemDeviceConfiguration(tag: "firecrab")
        fileSystem.share = VZSingleDirectoryShare(directory: sharedDirectory)

        let configuration = VZVirtualMachineConfiguration()
        configuration.platform = platform
        configuration.bootLoader = bootLoader
        configuration.cpuCount = options.cpuCount
        configuration.memorySize = memorySize
        configuration.storageDevices = [
            VZVirtioBlockDeviceConfiguration(attachment: osAttachment),
            VZVirtioBlockDeviceConfiguration(attachment: dataAttachment),
        ]
        configuration.networkDevices = [network]
        configuration.serialPorts = [console]
        configuration.directorySharingDevices = [fileSystem]
        configuration.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        configuration.memoryBalloonDevices = [
            VZVirtioTraditionalMemoryBalloonDeviceConfiguration(),
        ]
        configuration.socketDevices = [VZVirtioSocketDeviceConfiguration()]
        try configuration.validate()
        return configuration
    }
}

private func configurationDiagnostic(
    id: String,
    detail: String,
    validation: () throws -> Void
) -> Diagnostic {
    do {
        try validation()
        return Diagnostic(id: id, status: .pass, detail: detail, fix: nil)
    } catch {
        return Diagnostic(id: id, status: .fail, detail: "\(detail) — \(error)", fix: nil)
    }
}

private func validateDirectory(_ url: URL, label: String) throws {
    let values: URLResourceValues
    do {
        values = try url.resourceValues(forKeys: [.isDirectoryKey])
    } catch {
        throw MicroManagerError.invalidArtifact(
            "\(label) is not readable: \(error.localizedDescription)"
        )
    }
    guard values.isDirectory == true else {
        throw MicroManagerError.invalidArtifact("\(label) is not a directory")
    }
    guard FileManager.default.isWritableFile(atPath: url.path) else {
        throw MicroManagerError.invalidArtifact("\(label) is not writable")
    }
}

private func validatedMemorySize(_ options: VMOptions) throws -> UInt64 {
    guard
        options.cpuCount >= VZVirtualMachineConfiguration.minimumAllowedCPUCount,
        options.cpuCount <= VZVirtualMachineConfiguration.maximumAllowedCPUCount
    else {
        throw MicroManagerError.argument(
            "managed CPU count must be between \(VZVirtualMachineConfiguration.minimumAllowedCPUCount) and \(VZVirtualMachineConfiguration.maximumAllowedCPUCount) on this host"
        )
    }
    let (memorySize, overflow) = options.memorySizeMiB.multipliedReportingOverflow(
        by: 1024 * 1024
    )
    guard !overflow else {
        throw MicroManagerError.argument("managed memory size is too large")
    }
    guard
        memorySize >= VZVirtualMachineConfiguration.minimumAllowedMemorySize,
        memorySize <= VZVirtualMachineConfiguration.maximumAllowedMemorySize
    else {
        throw MicroManagerError.argument("managed memory size is unsupported on this host")
    }
    return memorySize
}

func validateArm64Kernel(_ url: URL) throws {
    let handle: FileHandle
    do {
        handle = try FileHandle(forReadingFrom: url)
    } catch {
        throw MicroManagerError.invalidArtifact(
            "kernel is not readable at \(url.path): \(error.localizedDescription)"
        )
    }
    defer { try? handle.close() }

    let header: Data
    do {
        header = try handle.read(upToCount: 64) ?? Data()
    } catch {
        throw MicroManagerError.invalidArtifact(
            "kernel header cannot be read at \(url.path): \(error.localizedDescription)"
        )
    }
    let arm64ImageMagic = Data([0x41, 0x52, 0x4d, 0x64])
    guard header.count >= 60, header.subdata(in: 56..<60) == arm64ImageMagic else {
        throw MicroManagerError.invalidArtifact(
            "kernel must be an uncompressed arm64 Linux Image with ARM64 header magic: \(url.path)"
        )
    }
}

private func validateRegularFile(_ url: URL, label: String, writable: Bool) throws {
    let values: URLResourceValues
    do {
        values = try url.resourceValues(forKeys: [.isRegularFileKey])
    } catch {
        throw MicroManagerError.invalidArtifact(
            "\(label) is not readable at \(url.path): \(error.localizedDescription)"
        )
    }
    guard values.isRegularFile == true else {
        throw MicroManagerError.invalidArtifact("\(label) is not a regular file: \(url.path)")
    }
    if writable && !FileManager.default.isWritableFile(atPath: url.path) {
        throw MicroManagerError.invalidArtifact("\(label) is not writable: \(url.path)")
    }
}

private func validateSeparateDisks(_ osDiskURL: URL, _ dataDiskURL: URL) throws {
    if osDiskURL == dataDiskURL {
        throw MicroManagerError.invalidArtifact(
            "OS disk and persistent data disk must be different files"
        )
    }

    let osIdentifier: NSObject?
    let dataIdentifier: NSObject?
    do {
        osIdentifier = try osDiskURL.resourceValues(forKeys: [.fileResourceIdentifierKey])
            .fileResourceIdentifier as? NSObject
        dataIdentifier = try dataDiskURL.resourceValues(forKeys: [.fileResourceIdentifierKey])
            .fileResourceIdentifier as? NSObject
    } catch {
        throw MicroManagerError.invalidArtifact(
            "OS and data disk identity could not be compared: \(error.localizedDescription)"
        )
    }
    if let osIdentifier, let dataIdentifier, osIdentifier.isEqual(dataIdentifier) {
        throw MicroManagerError.invalidArtifact(
            "OS disk and persistent data disk must not be hard links to the same file"
        )
    }
}
