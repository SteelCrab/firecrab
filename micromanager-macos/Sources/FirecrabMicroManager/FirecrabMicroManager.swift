import Darwin
import Foundation

private let usage = """
Usage: firecrab-micromanager-macos <command>

Commands:
  doctor [--json]  Detect macOS, entitlement, and nested virtualization support
  validate         Show and validate the managed VM configuration
  provision        EFI boot the Debian image and run first-boot provisioning
  run              Boot the managed VM and attach its serial console

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
            case "validate", "provision", "run":
                let vmArguments = Array(arguments.dropFirst())
                if vmArguments == ["--help"] || vmArguments == ["-h"] {
                    print(usage)
                    return
                }
                guard vmArguments.isEmpty else {
                    throw MicroManagerError.argument("\(command) does not accept options")
                }
                let options = VMOptions.managedDefaults()
                if command == "validate" {
                    let checks = ManagementVMConfiguration.inspect(options: options)
                    print(HostCapabilities.renderChecks(checks))
                    exit(checks.contains { $0.status == .fail } ? 1 : 0)
                } else if command == "provision" {
                    let configuration = try ManagementVMConfiguration.makeProvisioning(
                        options: options
                    )
                    try await runManagementVM(configuration: configuration)
                } else {
                    let configuration = try ManagementVMConfiguration.make(options: options)
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
