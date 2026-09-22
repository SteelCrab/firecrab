import Foundation
import Testing
@testable import FirecrabMicroManager

@Test
func supportedInputsAreReadyButKeepEndToEndGatesVisible() {
    let report = HostCapabilities.report(
        from: CapabilityInputs(
            osVersion: OperatingSystemVersion(majorVersion: 15, minorVersion: 0, patchVersion: 0),
            osVersionString: "Version 15.0",
            architecture: "arm64",
            virtualizationSupported: true,
            entitlementPresent: true,
            nestedVirtualizationSupported: true
        )
    )

    #expect(report.ready)
    #expect(report.checks.allSatisfy { $0.status == .pass })
    #expect(report.validationGates.contains { $0.contains("/dev/kvm") })
    #expect(report.validationGates.contains { $0.contains("Firecracker") })

    let human = HostCapabilities.renderHuman(report)
    let lines = human.split(separator: "\n")
    #expect(lines.count == report.checks.count)
    #expect(lines.allSatisfy { $0.hasPrefix("[PASS]") })
    #expect(!human.contains("feasibility"))
    #expect(!human.contains("fix:"))
    #expect(!human.contains("UNVALIDATED"))
}

@Test
func unsupportedNestedVirtualizationFailsWithActionableDiagnostic() {
    let report = HostCapabilities.report(
        from: CapabilityInputs(
            osVersion: OperatingSystemVersion(majorVersion: 15, minorVersion: 0, patchVersion: 0),
            osVersionString: "Version 15.0",
            architecture: "arm64",
            virtualizationSupported: true,
            entitlementPresent: true,
            nestedVirtualizationSupported: false
        )
    )

    #expect(!report.ready)
    let nested = report.checks.first { $0.id == "nested_virtualization" }
    #expect(nested?.status == .fail)
    #expect(nested?.fix?.contains("M3-or-later") == true)

    let human = HostCapabilities.renderHuman(report)
    #expect(human.contains("[FAILED] nested_virtualization:"))
    #expect(!human.contains("M3-or-later"))
}

@Test
func managedDefaultsUseTheMacApplicationSupportLayout() {
    let options = VMOptions.managedDefaults(
        environment: [:],
        homeDirectory: URL(fileURLWithPath: "/Users/example", isDirectory: true)
    )

    #expect(
        options.managedHomeURL.path
            == "/Users/example/Library/Application Support/Firecrab/micromanager"
    )
    #expect(options.kernelURL.lastPathComponent == "Image")
    #expect(options.initialRamdiskURL?.lastPathComponent == "initrd.img")
    #expect(options.osDiskURL.lastPathComponent == "debian-system.raw")
    #expect(options.dataDiskURL.lastPathComponent == "firecrab-data.raw")
    #expect(options.osDiskURL.deletingLastPathComponent().lastPathComponent == "system")
    #expect(options.dataDiskURL.deletingLastPathComponent().lastPathComponent == "data")
}

@Test
func managedHomeEnvironmentOverrideWins() {
    let options = VMOptions.managedDefaults(
        environment: ["FIRECRAB_MICROMANAGER_HOME": "/tmp/firecrab-managed"],
        homeDirectory: URL(fileURLWithPath: "/ignored", isDirectory: true)
    )

    #expect(options.managedHomeURL.path == "/tmp/firecrab-managed")
}

@Test
func validateReportsEveryManagedSettingWithoutArguments() throws {
    let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent(UUID().uuidString, isDirectory: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    let options = VMOptions.managedDefaults(
        environment: ["FIRECRAB_MICROMANAGER_HOME": directory.path]
    )

    let checks = ManagementVMConfiguration.inspect(options: options)
    #expect(checks.first { $0.id == "managed_home" }?.status == .fail)
    #expect(checks.first { $0.id == "kernel" }?.status == .fail)
    #expect(checks.first { $0.id == "initrd" }?.status == .fail)
    #expect(checks.first { $0.id == "os_disk" }?.status == .fail)
    #expect(checks.first { $0.id == "data_disk" }?.status == .fail)
    #expect(checks.first { $0.id == "disk_separation" }?.status == .fail)
    #expect(checks.first { $0.id == "resources" }?.status == .pass)
    #expect(checks.contains { $0.id == "virtual_machine" } == false)

    let human = HostCapabilities.renderChecks(checks)
    #expect(human.contains("[FAILED] managed_home:"))
    #expect(human.contains("[PASS] resources:"))
    #expect(human.contains(directory.path))
}

@Test
func humanDiagnosticsUsePassWarningAndFailedLabels() {
    let checks = [
        Diagnostic(id: "ok", status: .pass, detail: "ready", fix: nil),
        Diagnostic(id: "careful", status: .warning, detail: "review", fix: nil),
        Diagnostic(id: "broken", status: .fail, detail: "missing", fix: nil),
    ]

    #expect(
        HostCapabilities.renderChecks(checks)
            == "[PASS] ok: ready\n[WARNING] careful: review\n[FAILED] broken: missing"
    )
}

@Test
func managedCommandLineUsesProvisionedRootPartuuid() throws {
    let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent(UUID().uuidString, isDirectory: true)
    let runtime = directory.appendingPathComponent("runtime", isDirectory: true)
    try FileManager.default.createDirectory(at: runtime, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    try "571e75ed-4438-40ec-b68e-d5a97df8f5ae\n".write(
        to: runtime.appendingPathComponent("root-partuuid"),
        atomically: true,
        encoding: .utf8
    )

    let provisioned = VMOptions.managedDefaults(
        environment: ["FIRECRAB_MICROMANAGER_HOME": directory.path]
    )
    #expect(
        provisioned.commandLine.contains(
            "root=PARTUUID=571e75ed-4438-40ec-b68e-d5a97df8f5ae"
        )
    )

    let fallback = VMOptions.managedDefaults(
        environment: ["FIRECRAB_MICROMANAGER_HOME": directory.appendingPathComponent("missing").path]
    )
    #expect(fallback.commandLine.contains("root=/dev/vda1"))
}

@Test
func kernelValidationRequiresRawArm64ImageMagic() throws {
    let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }

    let validKernel = directory.appendingPathComponent("Image")
    var header = Data(repeating: 0, count: 64)
    header.replaceSubrange(56..<60, with: [0x41, 0x52, 0x4d, 0x64])
    try header.write(to: validKernel)
    try validateArm64Kernel(validKernel)

    let invalidKernel = directory.appendingPathComponent("vmlinuz")
    try Data().write(to: invalidKernel)
    #expect(throws: MicroManagerError.self) {
        try validateArm64Kernel(invalidKernel)
    }
}
