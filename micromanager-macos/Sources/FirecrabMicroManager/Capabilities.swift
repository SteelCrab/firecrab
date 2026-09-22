import Foundation
import Security
@preconcurrency import Virtualization

enum DiagnosticStatus: String, Codable, Sendable {
    case pass
    case warning
    case fail
}

struct Diagnostic: Codable, Equatable, Sendable {
    let id: String
    let status: DiagnosticStatus
    let detail: String
    let fix: String?
}

struct CapabilityReport: Codable, Equatable, Sendable {
    let product: String
    let platform: String
    let osVersion: String
    let architecture: String
    let ready: Bool
    let checks: [Diagnostic]
    let validationGates: [String]
}

struct CapabilityInputs: Sendable {
    let osVersion: OperatingSystemVersion
    let osVersionString: String
    let architecture: String
    let virtualizationSupported: Bool
    let entitlementPresent: Bool
    let nestedVirtualizationSupported: Bool

    static func live() -> CapabilityInputs {
        #if arch(arm64)
        let architecture = "arm64"
        #elseif arch(x86_64)
        let architecture = "x86_64"
        #else
        let architecture = "unknown"
        #endif

        let nestedVirtualizationSupported: Bool
        if #available(macOS 15.0, *) {
            nestedVirtualizationSupported =
                VZGenericPlatformConfiguration.isNestedVirtualizationSupported
        } else {
            nestedVirtualizationSupported = false
        }

        return CapabilityInputs(
            osVersion: ProcessInfo.processInfo.operatingSystemVersion,
            osVersionString: ProcessInfo.processInfo.operatingSystemVersionString,
            architecture: architecture,
            virtualizationSupported: VZVirtualMachine.isSupported,
            entitlementPresent: currentProcessHasVirtualizationEntitlement(),
            nestedVirtualizationSupported: nestedVirtualizationSupported
        )
    }
}

private func currentProcessHasVirtualizationEntitlement() -> Bool {
    guard
        let task = SecTaskCreateFromSelf(nil),
        let value = SecTaskCopyValueForEntitlement(
            task,
            "com.apple.security.virtualization" as CFString,
            nil
        ) as? NSNumber
    else {
        return false
    }
    return value.boolValue
}

enum HostCapabilities {
    static let validationGates = [
        "architecture-matched Debian with systemd boots",
        "the management guest exposes usable /dev/kvm",
        "an actual Firecracker workload microVM boots",
        "guest networking, console, shutdown, and recovery pass",
    ]

    static func report(from inputs: CapabilityInputs = .live()) -> CapabilityReport {
        let osSupported = inputs.osVersion.majorVersion >= 15
        let architectureSupported = inputs.architecture == "arm64"

        let checks = [
            Diagnostic(
                id: "macos_version",
                status: osSupported ? .pass : .fail,
                detail: osSupported
                    ? "macOS 15 or later is available (\(inputs.osVersionString))."
                    : "macOS 15 or later is required (\(inputs.osVersionString)).",
                fix: osSupported ? nil : "Upgrade macOS before using microManager."
            ),
            Diagnostic(
                id: "host_architecture",
                status: architectureSupported ? .pass : .fail,
                detail: architectureSupported
                    ? "Apple silicon arm64 host detected."
                    : "Unsupported macOS architecture: \(inputs.architecture).",
                fix: architectureSupported
                    ? nil
                    : "Use an Apple silicon Mac; the release does not support Intel macOS."
            ),
            Diagnostic(
                id: "virtualization_entitlement",
                status: inputs.entitlementPresent ? .pass : .fail,
                detail: inputs.entitlementPresent
                    ? "The helper carries com.apple.security.virtualization."
                    : "The helper is missing com.apple.security.virtualization.",
                fix: inputs.entitlementPresent
                    ? nil
                    : "Install the signed release helper or rebuild it with scripts/build-micromanager-macos.sh."
            ),
            Diagnostic(
                id: "virtualization_framework",
                status: inputs.virtualizationSupported ? .pass : .fail,
                detail: inputs.virtualizationSupported
                    ? "Virtualization.framework can create virtual machines."
                    : "Virtualization.framework cannot create a virtual machine in this process.",
                fix: inputs.virtualizationSupported
                    ? nil
                    : "Check the virtualization entitlement and host security policy."
            ),
            Diagnostic(
                id: "nested_virtualization",
                status: inputs.nestedVirtualizationSupported ? .pass : .fail,
                detail: inputs.nestedVirtualizationSupported
                    ? "Apple nested virtualization is available and will be enabled explicitly."
                    : "Apple nested virtualization is unavailable on this host or configuration.",
                fix: inputs.nestedVirtualizationSupported
                    ? nil
                    : "Use a supported M3-or-later Mac and verify the current macOS configuration."
            ),
        ]

        return CapabilityReport(
            product: "microManager",
            platform: "macOS",
            osVersion: inputs.osVersionString,
            architecture: inputs.architecture,
            ready: !checks.contains { $0.status == .fail },
            checks: checks,
            validationGates: validationGates
        )
    }

    static func renderHuman(_ report: CapabilityReport) -> String {
        renderChecks(report.checks)
    }

    static func renderChecks(_ checks: [Diagnostic]) -> String {
        checks.map { check in
            let label = switch check.status {
            case .pass: "PASS"
            case .warning: "WARNING"
            case .fail: "FAILED"
            }
            return "[\(label)] \(check.id): \(check.detail)"
        }.joined(separator: "\n")
    }
}
