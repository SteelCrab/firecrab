import Foundation
import Testing
@testable import FirecrabMicroManager

private func fixture() throws -> (URL, VMOptions, URL) {
    let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    for name in ["system", "data", "runtime/repairs/test"] {
        try FileManager.default.createDirectory(at: home.appendingPathComponent(name),
            withIntermediateDirectories: true)
    }
    let options = VMOptions.managedDefaults(environment: ["FIRECRAB_MICROMANAGER_HOME": home.path])
    try Data("original".utf8).write(to: options.osDiskURL)
    try Data("data".utf8).write(to: options.dataDiskURL)
    let working = home.appendingPathComponent("runtime/repairs/test/debian-system.repaired.raw")
    try Data("copy".utf8).write(to: working)
    return (home, options, working)
}

@Test
func recoveryRejectsOriginalAliasedAndSymlinkedDisks() throws {
    let (home, options, working) = try fixture()
    defer { try? FileManager.default.removeItem(at: home) }
    #expect(throws: MicroManagerError.self) { try recoveryOptions(options, disk: options.osDiskURL) }
    try FileManager.default.removeItem(at: working)
    try FileManager.default.linkItem(at: options.osDiskURL, to: working)
    #expect(throws: MicroManagerError.self) { try recoveryOptions(options, disk: working) }
    try FileManager.default.removeItem(at: working)
    let key = home.appendingPathComponent("runtime/manager_ed25519")
    try Data("private fixture key".utf8).write(to: key)
    try FileManager.default.linkItem(at: key, to: working)
    #expect(throws: MicroManagerError.self) { try recoveryOptions(options, disk: working) }
    #expect(try Data(contentsOf: key) == Data("private fixture key".utf8))
    try FileManager.default.removeItem(at: working)
    try FileManager.default.createSymbolicLink(at: working, withDestinationURL: options.osDiskURL)
    #expect(throws: MicroManagerError.self) { try recoveryOptions(options, disk: working) }
}

@Test
func recoveryBootStopsBeforeMountingTheRootAndUsesOnlyTheCopy() throws {
    let (home, options, working) = try fixture()
    defer { try? FileManager.default.removeItem(at: home) }
    let recovery = try recoveryOptions(options, disk: working)
    #expect(recovery.osDiskURL == working.standardizedFileURL)
    #expect(recovery.commandLine.contains("break=premount"))
    #expect(recovery.memorySizeMiB == 1024)
    #expect(recovery.dataDiskURL == options.dataDiskURL)
}

@Test
func runningVMAndOSRecoveryCannotBothAcquireTheHomeLock() throws {
    let (home, _, _) = try fixture()
    defer { try? FileManager.default.removeItem(at: home) }
    let first = try VMRunLock(home: home)
    #expect(throws: MicroManagerError.self) { try VMRunLock(home: home) }
    first.close()
    let second = try VMRunLock(home: home)
    second.close()
}

@Test @MainActor
func forcedStopCompletionUnblocksTheObserverWithoutADelegateCallback() async throws {
    let observer = VirtualMachineStopObserver()
    let waiting = Task { try await observer.wait() }
    await Task.yield()
    observer.finish(.success(()))
    try await waiting.value
    observer.finish(.failure(MicroManagerError.invalidArtifact("late callback")))
    try await observer.wait()
}

@Test @MainActor
func consoleAcceptsOnlyACompleteFilesystemResultAndRequiresVerification() throws {
    let pipe = Pipe()
    let console = OSRepairConsole(input: pipe.fileHandleForWriting)
    console.consume(Data("printf '\\n\(console.marker):%s:%s:%s\\n'\n".utf8))
    #expect(console.result == nil)
    console.consume(Data("\(console.marker):4:1:".utf8))
    #expect(console.result == nil)
    console.consume(Data("0\n".utf8))
    #expect(console.result?.verified == true)
    let serial = OSRepairConsole(input: pipe.fileHandleForWriting)
    serial.consume(Data("fsck finished\r\n\(serial.marker):0:-1:".utf8))
    #expect(serial.result == nil)
    serial.consume(Data("0\r".utf8))
    serial.consume(Data("\n".utf8))
    #expect(serial.result == OSRepairResult(check_exit: 0, repair_exit: nil,
        verify_exit: 0, changed: false))
    #expect(OSRepairResult(check_exit: 4, repair_exit: 1, verify_exit: 4, changed: true).verified == false)
    #expect(OSRepairResult(check_exit: 0, repair_exit: nil, verify_exit: 0, changed: false).verified)
    #expect(OSRepairResult(check_exit: 8, repair_exit: 0, verify_exit: 0, changed: true).verified == false)
}

@Test
func recoveryReferenceRejectsAliasesAndRequiresThePrivateDirectory() throws {
    let (home, options, working) = try fixture()
    defer { try? FileManager.default.removeItem(at: home) }
    let reference = working.deletingLastPathComponent().appendingPathComponent("debian-system.reference.raw")
    try Data("cached image".utf8).write(to: reference)
    #expect(try recoveryReference(options, disk: working, reference: reference) == reference)
    try FileManager.default.removeItem(at: reference)
    try FileManager.default.linkItem(at: working, to: reference)
    #expect(throws: MicroManagerError.self) { try recoveryReference(options, disk: working, reference: reference) }
    try FileManager.default.removeItem(at: reference)
    try FileManager.default.createSymbolicLink(at: reference, withDestinationURL: options.osDiskURL)
    #expect(throws: MicroManagerError.self) { try recoveryReference(options, disk: working, reference: reference) }
    #expect(throws: MicroManagerError.self) { try recoveryReference(options, disk: working, reference: options.osDiskURL) }
}

@Test @MainActor
func missingFilesCanChangeACleanFilesystemButStillRequireVerification() {
    let pipe = Pipe()
    let console = OSRepairConsole(input: pipe.fileHandleForWriting, restoreFiles: true)
    console.consume(Data("\(console.marker):0:-1:0:3\n".utf8))
    #expect(console.result?.restored_files == 3)
    #expect(console.result?.changed == true)
    #expect(console.result?.verified == true)
    #expect(OSRepairResult(check_exit: 0, repair_exit: nil, verify_exit: 8,
        changed: true, restored_files: 3).verified == false)
    #expect(OSRepairResult(check_exit: 0, repair_exit: nil, verify_exit: 0,
        changed: false, restored_files: 3).verified == false)
    #expect(OSRepairResult(check_exit: 0, repair_exit: nil, verify_exit: 0,
        changed: false, restored_files: -1).verified == false)
}
