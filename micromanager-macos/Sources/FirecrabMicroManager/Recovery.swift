import Darwin
import Foundation
@preconcurrency import Virtualization

final class VMRunLock {
    private var descriptor: Int32

    init(home: URL) throws {
        let url = home.appendingPathComponent("runtime/vm.lock")
        descriptor = open(url.path, O_RDWR | O_CREAT | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        guard descriptor >= 0 else {
            throw MicroManagerError.invalidArtifact("cannot open management VM lock: \(url.path)")
        }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            close()
            throw MicroManagerError.invalidArtifact("management VM or OS repair already owns \(url.path)")
        }
    }

    func close() {
        if descriptor >= 0 {
            Darwin.close(descriptor)
            descriptor = -1
        }
    }

    deinit { close() }
}

struct OSRepairResult: Codable, Equatable {
    let check_exit: Int
    let repair_exit: Int?
    let verify_exit: Int
    let changed: Bool
    var restored_files: Int = 0

    private enum CodingKeys: CodingKey {
        case check_exit, repair_exit, verify_exit, changed, restored_files
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(check_exit, forKey: .check_exit)
        if let repair_exit {
            try container.encode(repair_exit, forKey: .repair_exit)
        } else {
            try container.encodeNil(forKey: .repair_exit)
        }
        try container.encode(verify_exit, forKey: .verify_exit)
        try container.encode(changed, forKey: .changed)
        try container.encode(restored_files, forKey: .restored_files)
    }

    var verified: Bool {
        let checked = check_exit == 0 && repair_exit == nil
            || (1...7).contains(check_exit) && repair_exit.map { (0...3).contains($0) } == true
        return checked && verify_exit == 0 && restored_files >= 0
            && changed == (repair_exit != nil || restored_files > 0)
    }
}

func recoveryOptions(_ options: VMOptions, disk: URL) throws -> VMOptions {
    let home = options.managedHomeURL.standardizedFileURL.resolvingSymlinksInPath()
    let directory = home.appendingPathComponent("runtime/repairs", isDirectory: true).path + "/"
    let target = disk.standardizedFileURL
    guard target == target.resolvingSymlinksInPath(), target.path.hasPrefix(directory),
        target.lastPathComponent == "debian-system.repaired.raw"
    else {
        throw MicroManagerError.argument("repair-os accepts only a private OS copy under runtime/repairs")
    }
    let metadata = try FileManager.default.attributesOfItem(atPath: target.path)
    guard metadata[.type] as? FileAttributeType == .typeRegular,
        (metadata[.referenceCount] as? NSNumber)?.intValue == 1
    else {
        throw MicroManagerError.invalidArtifact("repair-os requires an unaliased regular OS copy")
    }
    try validateSeparateDisks(target, options.osDiskURL)
    try validateSeparateDisks(target, options.dataDiskURL)
    return VMOptions(
        managedHomeURL: options.managedHomeURL,
        kernelURL: options.kernelURL,
        initialRamdiskURL: options.initialRamdiskURL,
        osDiskURL: target,
        dataDiskURL: options.dataDiskURL,
        cpuCount: 2,
        memorySizeMiB: 1024,
        commandLine: options.commandLine + " break=premount TERM=dumb"
    )
}

func recoveryReference(_ options: VMOptions, disk: URL, reference: URL?) throws -> URL? {
    guard let reference else { return nil }
    let target = reference.standardizedFileURL
    guard target == target.resolvingSymlinksInPath(),
        target.deletingLastPathComponent() == disk.standardizedFileURL.deletingLastPathComponent(),
        target.lastPathComponent == "debian-system.reference.raw"
    else {
        throw MicroManagerError.argument("reference must be a private cached OS image beside the repair copy")
    }
    let metadata = try FileManager.default.attributesOfItem(atPath: target.path)
    guard metadata[.type] as? FileAttributeType == .typeRegular,
        (metadata[.referenceCount] as? NSNumber)?.intValue == 1
    else {
        throw MicroManagerError.invalidArtifact("reference must be an unaliased regular OS image")
    }
    for other in [disk, options.osDiskURL, options.dataDiskURL] {
        try validateSeparateDisks(target, other)
    }
    return target
}

@MainActor
final class OSRepairConsole {
    let marker = "FIRECRAB_FSCK_" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
    private let input: FileHandle
    private let restoreFiles: Bool
    private var buffer = ""
    private var sent = false
    private(set) var result: OSRepairResult?

    init(input: FileHandle, restoreFiles: Bool = false) {
        self.input = input
        self.restoreFiles = restoreFiles
    }

    private var restoreCommand: String {
        guard restoreFiles else { return "" }
        let encoded = Array(Data(osFileRestoreScript.utf8).base64EncodedString())
        let lines = stride(from: 0, to: encoded.count, by: 120).map {
            String(encoded[$0..<min($0 + 120, encoded.count)])
        }.joined(separator: "\n")
        // Keep each console line below the TTY input limit. The reference is
        // read-only; no data disk, host share, or network is attached.
        let directory = "/run/" + marker
        return """
        cat >/tmp/restore.b64 <<'FIRECRAB_SCRIPT'
        \(lines)
        FIRECRAB_SCRIPT
        base64 -d /tmp/restore.b64 >/tmp/restore.py
        if [ "$verify" -eq 0 ]; then
          if mount -t ext4 -o rw /dev/vda1 /root; then
            if mkdir -p /root\(directory)/reference && mount -t ext4 -o ro,noload /dev/vdb1 /root\(directory)/reference; then
              if mount --bind /dev /root/dev && cp /tmp/restore.py /root\(directory)/restore.py && chroot /root /usr/bin/python3 \(directory)/restore.py / \(directory)/reference >/tmp/restored; then restored=$(cat /tmp/restored); else verify=8; fi
              umount /root/dev || verify=8
              umount /root\(directory)/reference || verify=8
            else verify=8; fi
            rm -f /root\(directory)/restore.py
            rmdir /root\(directory)/reference /root\(directory) 2>/dev/null
            sync
            umount /root || verify=8
            if [ "$verify" -eq 0 ]; then /sbin/fsck.ext4 -f -n /dev/vda1; verify=$?; fi
          else verify=8; fi
        fi

        """
    }

    var command: String {
        // premount stops before the OS root is mounted. Require another
        // read-only check after both filesystem and packaged-file repairs.
        let check = "stty -echo; repair=-1; restored=0; "
            + "if [ ! -b /dev/vda1 ] || grep -qE '^/dev/(vda|root)|^[^ ]+ /root ' /proc/mounts; then check=8; verify=8; "
            + "else /sbin/fsck.ext4 -f -n /dev/vda1; check=$?; "
            + "if [ \"$check\" -gt 0 ] && [ \"$check\" -le 7 ]; then "
            + "/sbin/fsck.ext4 -f -y /dev/vda1; repair=$?; "
            + "if [ \"$repair\" -le 3 ]; then /sbin/fsck.ext4 -f -n /dev/vda1; verify=$?; else verify=$repair; fi; "
            + "else verify=$check; fi; fi;\n"
        return check + restoreCommand + "sync; "
            + "printf '\\n\(marker):%s:%s:%s:%s\\n' \"$check\" \"$repair\" \"$verify\" \"$restored\"; poweroff -f\n"
    }

    func consume(_ data: Data) {
        FileHandle.standardError.write(data)
        // Swift treats CRLF as one Character, so normalize serial-console line endings.
        buffer += String(decoding: data, as: UTF8.self).replacingOccurrences(of: "\r", with: "\n")
        if !sent && buffer.contains("(initramfs)") {
            sent = true
            input.write(Data(command.utf8))
        }
        while let newline = buffer.firstIndex(of: "\n") {
            let line = String(buffer[..<newline]).trimmingCharacters(in: .whitespacesAndNewlines)
            buffer.removeSubrange(...newline)
            guard line.hasPrefix(marker + ":") else { continue }
            let fields = line.split(separator: ":")
            guard (fields.count == 4 || fields.count == 5), let check = Int(fields[1]),
                let repair = Int(fields[2]), let verify = Int(fields[3]) else { continue }
            guard let restored = fields.count == 5 ? Int(fields[4]) : 0 else { continue }
            result = OSRepairResult(check_exit: check, repair_exit: repair < 0 ? nil : repair,
                verify_exit: verify, changed: repair >= 0 || restored > 0, restored_files: restored)
        }
        if buffer.count > 8192 { buffer = String(buffer.suffix(8192)) }
    }
}

@MainActor
func repairOS(options: VMOptions, disk: URL, reference: URL? = nil) async throws -> OSRepairResult {
    let reference = try recoveryReference(options, disk: disk, reference: reference)
    let options = try recoveryOptions(options, disk: disk)
    let input = Pipe()
    let output = Pipe()
    let console = OSRepairConsole(input: input.fileHandleForWriting, restoreFiles: reference != nil)
    let configuration = try ManagementVMConfiguration.make(options: options,
        consoleInput: input.fileHandleForReading, consoleOutput: output.fileHandleForWriting,
        repairingOS: true, referenceOS: reference)
    output.fileHandleForReading.readabilityHandler = { handle in
        let data = handle.availableData
        if !data.isEmpty {
            Task { @MainActor in console.consume(data) }
        }
    }
    defer {
        output.fileHandleForReading.readabilityHandler = nil
        try? input.fileHandleForWriting.close()
        try? output.fileHandleForWriting.close()
    }
    try await runManagementVM(configuration: configuration)
    // The delegate can report shutdown before the final serial chunk is queued.
    for _ in 0..<20 {
        if let result = console.result { return result }
        try await Task.sleep(for: .milliseconds(50))
    }
    throw MicroManagerError.invalidArtifact("OS repair VM stopped without a filesystem result")
}
