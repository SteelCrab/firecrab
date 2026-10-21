import Darwin
import Foundation

private let usage = """
Usage: firecrab-micromanager-macos <command>

Commands:
  doctor [--json]  Detect macOS, entitlement, and nested virtualization support
  validate         Show and validate the managed VM configuration
  provision        EFI boot the Debian image and run first-boot provisioning
  run              Boot the managed VM and attach its serial console
  resource-host    Report physical host capacity as JSON
  validate-resources <json> Validate an allocation without changing it
  repair-os <copy> [--reference <image>] Check filesystem and restore missing OS files

Environment:
  FIRECRAB_MICROMANAGER_HOME  Override the managed configuration root
"""

@main
struct FirecrabMicroManager {
    @MainActor
    static func main() async {
        let arguments = Array(CommandLine.arguments.dropFirst())
        guard let command = arguments.first else {
            print(usage)
            exit(2)
        }

        do {
            switch command {
            case "resource-host":
                guard arguments.count == 1 else { throw MicroManagerError.argument("resource-host accepts no options") }
                let data = try resourceHostReport(home: VMOptions.managedDefaults().managedHomeURL)
                print(String(decoding: data, as: UTF8.self))
            case "validate-resources":
                guard arguments.count == 2 else { throw MicroManagerError.argument("validate-resources requires a JSON path") }
                let data = try Data(contentsOf: URL(fileURLWithPath: arguments[1]))
                try JSONDecoder().decode(ManagerAllocation.self, from: data).validate()
            case "help", "--help", "-h":
                print(usage)
            case "doctor":
                let doctorArguments = Array(arguments.dropFirst())
                guard doctorArguments.isEmpty || doctorArguments == ["--json"] else {
                    throw MicroManagerError.argument("doctor accepts only --json")
                }
                let report = HostCapabilities.report()
                if doctorArguments == ["--json"] {
                    let encoder = JSONEncoder()
                    encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
                    let data = try encoder.encode(report)
                    print(String(decoding: data, as: UTF8.self))
                } else {
                    print(HostCapabilities.renderHuman(report))
                }
                exit(report.ready ? 0 : 1)
            case "repair-os":
                let vmArguments = Array(arguments.dropFirst())
                if vmArguments == ["--help"] || vmArguments == ["-h"] {
                    print(usage)
                    return
                }
                guard (vmArguments.count == 1 || vmArguments.count == 3),
                    vmArguments[0].hasPrefix("/"),
                    vmArguments.count == 1 || (vmArguments[1] == "--reference" && vmArguments[2].hasPrefix("/"))
                else {
                    throw MicroManagerError.argument("repair-os requires an absolute private OS copy path")
                }
                let result = try await repairOS(options: VMOptions.managedDefaults(),
                    disk: URL(fileURLWithPath: vmArguments[0]),
                    reference: vmArguments.count == 3 ? URL(fileURLWithPath: vmArguments[2]) : nil)
                let data = try JSONEncoder().encode(result)
                print(String(decoding: data, as: UTF8.self))
                exit(result.verified ? 0 : 1)
            case "validate", "provision", "run":
                let vmArguments = Array(arguments.dropFirst())
                if vmArguments == ["--help"] || vmArguments == ["-h"] {
                    print(usage)
                    return
                }
                guard vmArguments.isEmpty else {
                    throw MicroManagerError.argument("\(command) does not accept options")
                }
                let options = try VMOptions.configured()
                if command == "validate" {
                    let checks = ManagementVMConfiguration.inspect(options: options)
                    print(HostCapabilities.renderChecks(checks))
                    exit(checks.contains { $0.status == .fail } ? 1 : 0)
                } else if command == "provision" {
                    let lock = try VMRunLock(home: options.managedHomeURL)
                    defer { lock.close() }
                    let configuration = try ManagementVMConfiguration.makeProvisioning(
                        options: options
                    )
                    try await runManagementVM(configuration: configuration)
                } else {
                    let lock = try VMRunLock(home: options.managedHomeURL)
                    defer { lock.close() }
                    let configuration = try ManagementVMConfiguration.make(options: options)
                    try options.publishActiveResources()
                    try await runManagementVM(configuration: configuration)
                }
            default:
                throw MicroManagerError.argument("unknown command: \(command)")
            }
        } catch {
            FileHandle.standardError.write(Data("microManager: \(error)\n".utf8))
            exit(2)
        }
    }
}
