use std::collections::BTreeMap;
use std::process::Command as ProcessCommand;

use clap::Subcommand;
use serde::Serialize;

const VALIDATION_GATES: [&str; 4] = [
    "architecture-matched Debian with systemd boots",
    "the management guest exposes usable /dev/kvm",
    "an actual Firecracker workload microVM boots",
    "guest networking, console, shutdown, and recovery pass",
];

/// Reports `key=value` lines so the parser never depends on guest locale.
const DISTRO_PROBE: &str = "printf 'kernel=%s\\n' \"$(uname -r)\"; \
printf 'uptime=%s\\n' \"$(cut -d. -f1 /proc/uptime)\"; \
[ -e /dev/kvm ] && printf 'kvm=present\\n'; \
(exec 3<>/dev/kvm) 2>/dev/null && printf 'kvm_open=ok\\n'; \
grep -Eqw 'vmx|svm' /proc/cpuinfo && printf 'nested=yes\\n'; \
exit 0";

/// `kvm_intel` loads about 25 seconds into the WSL2 utility VM's boot, so a probe
/// run right after a cold start sees no `/dev/kvm` on a host that supports it.
const KVM_SETTLE_SECONDS: u64 = 60;

mod install;

#[derive(Subcommand)]
pub enum Command {
    /// Detect WSL2 and the nested virtualization the managed Debian guest needs.
    Doctor {
        /// Emit a machine-readable capability report.
        #[arg(long)]
        json: bool,
    },
    /// Import the managed Debian distribution and place the guest binaries.
    Install,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not render the capability report: {0}")]
    Render(#[from] serde_json::Error),
    #[error(transparent)]
    Install(#[from] install::Error),
}

pub fn run(command: Command) -> Result<i32, Error> {
    match command {
        Command::Doctor { json } => {
            let report = report(Inputs::live());
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", render_human(&report));
            }
            Ok(i32::from(!report.ready))
        }
        Command::Install => Ok(install::run()?),
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pass,
    Warning,
    Fail,
}

#[derive(Serialize, PartialEq, Eq, Debug)]
struct Diagnostic {
    id: &'static str,
    status: Status,
    detail: String,
    fix: Option<String>,
}

#[derive(Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
struct CapabilityReport {
    product: &'static str,
    platform: &'static str,
    os_version: String,
    architecture: &'static str,
    ready: bool,
    checks: Vec<Diagnostic>,
    validation_gates: [&'static str; 4],
}

struct Inputs {
    windows_version: String,
    architecture: &'static str,
    wsl_version: Option<String>,
    wsl_kernel: Option<String>,
    default_distro: Option<String>,
    distro: BTreeMap<String, String>,
}

impl Inputs {
    fn live() -> Self {
        let wsl = console_output("wsl.exe", &["--version"]).unwrap_or_default();
        let default_distro = console_output("wsl.exe", &["--list", "--quiet"])
            .as_deref()
            .and_then(first_distro);
        let distro = console_output("wsl.exe", &["-e", "sh", "-c", DISTRO_PROBE])
            .as_deref()
            .map(parse_probe)
            .unwrap_or_default();
        Self {
            windows_version: console_output("cmd.exe", &["/c", "ver"])
                .as_deref()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            architecture: std::env::consts::ARCH,
            wsl_version: field(&wsl, "WSL version"),
            wsl_kernel: field(&wsl, "Kernel version"),
            default_distro,
            distro,
        }
    }
}

fn console_output(program: &str, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| decode_console(&output.stdout))
}

/// `wsl.exe` writes UTF-16LE, while `cmd.exe` writes the OEM code page.
fn decode_console(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let utf16le = body.len() >= 2 && body.len().is_multiple_of(2) && body[1] == 0;
    if !utf16le {
        return String::from_utf8_lossy(body).into_owned();
    }
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn field(text: &str, label: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim() == label)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn first_distro(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn parse_probe(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn major_version(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

fn wsl_version_check(version: Option<&str>) -> Diagnostic {
    let Some(version) = version else {
        return Diagnostic {
            id: "wsl_version",
            status: Status::Fail,
            detail: "`wsl --version` did not report an installed WSL.".to_string(),
            fix: Some("Install WSL with `wsl --install`.".to_string()),
        };
    };
    if major_version(version) < Some(2) {
        return Diagnostic {
            id: "wsl_version",
            status: Status::Fail,
            detail: format!("WSL {version} is older than the required WSL 2."),
            fix: Some("Update WSL with `wsl --update`.".to_string()),
        };
    }
    Diagnostic {
        id: "wsl_version",
        status: Status::Pass,
        detail: format!("WSL {version} is installed."),
        fix: None,
    }
}

fn distribution_check(distribution: Option<&str>) -> Diagnostic {
    let Some(distribution) = distribution else {
        return Diagnostic {
            id: "wsl_distribution",
            status: Status::Fail,
            detail: "WSL has no installed distribution.".to_string(),
            fix: Some("Install one with `wsl --install -d Debian`.".to_string()),
        };
    };
    Diagnostic {
        id: "wsl_distribution",
        status: Status::Pass,
        detail: format!("The default distribution is {distribution}."),
        fix: None,
    }
}

fn kvm_check(probe: &BTreeMap<String, String>, kernel: &str) -> Diagnostic {
    if !probe.contains_key("kvm") {
        let uptime = probe.get("uptime").and_then(|value| value.parse().ok());
        if uptime.is_some_and(|seconds: u64| seconds < KVM_SETTLE_SECONDS) {
            return Diagnostic {
                id: "kvm_device",
                status: Status::Warning,
                detail: "/dev/kvm has not appeared yet; the distribution is still starting."
                    .to_string(),
                fix: Some("Wait for the distribution to settle, then check again.".to_string()),
            };
        }
        return Diagnostic {
            id: "kvm_device",
            status: Status::Fail,
            detail: format!("/dev/kvm is missing from WSL2 kernel {kernel}."),
            fix: Some(
                "Enable nested virtualization for WSL2 and update to a kernel that builds KVM."
                    .to_string(),
            ),
        };
    }
    if !probe.contains_key("kvm_open") {
        return Diagnostic {
            id: "kvm_device",
            status: Status::Warning,
            detail: "/dev/kvm exists but this user cannot open it.".to_string(),
            fix: Some(
                "Join the kvm group inside the distribution: `sudo usermod -aG kvm $USER`."
                    .to_string(),
            ),
        };
    }
    Diagnostic {
        id: "kvm_device",
        status: Status::Pass,
        detail: format!("/dev/kvm opens inside WSL2 on kernel {kernel}."),
        fix: None,
    }
}

fn nested_virtualization_check(probe: &BTreeMap<String, String>) -> Diagnostic {
    if !probe.contains_key("nested") {
        return Diagnostic {
            id: "nested_virtualization",
            status: Status::Fail,
            detail: "The WSL2 guest reports no hardware virtualization extensions.".to_string(),
            fix: Some(
                "Enable virtualization in firmware, and nested virtualization if this host is itself a VM."
                    .to_string(),
            ),
        };
    }
    Diagnostic {
        id: "nested_virtualization",
        status: Status::Pass,
        detail: "The WSL2 guest sees hardware virtualization extensions.".to_string(),
        fix: None,
    }
}

fn report(inputs: Inputs) -> CapabilityReport {
    let kernel = inputs
        .distro
        .get("kernel")
        .or(inputs.wsl_kernel.as_ref())
        .map(String::as_str)
        .unwrap_or("unknown");
    let checks = vec![
        wsl_version_check(inputs.wsl_version.as_deref()),
        distribution_check(inputs.default_distro.as_deref()),
        kvm_check(&inputs.distro, kernel),
        nested_virtualization_check(&inputs.distro),
    ];
    CapabilityReport {
        product: "microManager",
        platform: "Windows",
        ready: !checks.iter().any(|check| check.status == Status::Fail),
        checks,
        os_version: inputs.windows_version,
        architecture: inputs.architecture,
        validation_gates: VALIDATION_GATES,
    }
}

fn render_human(report: &CapabilityReport) -> String {
    report
        .checks
        .iter()
        .map(|check| {
            let label = match check.status {
                Status::Pass => "PASS",
                Status::Warning => "WARNING",
                Status::Fail => "FAILED",
            };
            format!("[{label}] {}: {}", check.id, check.detail)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_inputs() -> Inputs {
        Inputs {
            windows_version: "Microsoft Windows [Version 10.0.26200.1]".to_string(),
            architecture: "x86_64",
            wsl_version: Some("2.7.14.0".to_string()),
            wsl_kernel: Some("6.18.33.2-2".to_string()),
            default_distro: Some("Debian".to_string()),
            distro: parse_probe(
                "kernel=6.18.33.2-microsoft-standard-WSL2\nuptime=900\nkvm=present\nkvm_open=ok\nnested=yes\n",
            ),
        }
    }

    fn check<'a>(report: &'a CapabilityReport, id: &str) -> &'a Diagnostic {
        report
            .checks
            .iter()
            .find(|check| check.id == id)
            .expect("check is reported")
    }

    #[test]
    fn decodes_utf16_wsl_output() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "WSL version: 2.7.14.0".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(
            field(&decode_console(&bytes), "WSL version").as_deref(),
            Some("2.7.14.0")
        );
    }

    #[test]
    fn decodes_plain_utf8_output() {
        assert_eq!(decode_console(b"Debian\n"), "Debian\n");
    }

    #[test]
    fn reads_the_first_installed_distribution() {
        assert_eq!(
            first_distro("\nDebian\nUbuntu\n").as_deref(),
            Some("Debian")
        );
        assert_eq!(first_distro("  \n"), None);
    }

    #[test]
    fn a_fully_capable_host_is_ready() {
        let report = report(ready_inputs());
        assert!(report.ready);
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status == Status::Pass)
        );
        assert_eq!(check(&report, "kvm_device").fix, None);
    }

    #[test]
    fn an_unopenable_kvm_device_warns_without_blocking() {
        let mut inputs = ready_inputs();
        inputs.distro.remove("kvm_open");
        let report = report(inputs);
        assert_eq!(check(&report, "kvm_device").status, Status::Warning);
        assert!(report.ready);
    }

    #[test]
    fn a_cold_started_distribution_warns_instead_of_failing() {
        let mut inputs = ready_inputs();
        inputs.distro.remove("kvm");
        inputs.distro.remove("kvm_open");
        inputs.distro.insert("uptime".to_string(), "12".to_string());
        let report = report(inputs);
        assert_eq!(check(&report, "kvm_device").status, Status::Warning);
        assert!(
            report.ready,
            "a still-starting guest must not block install"
        );
    }

    #[test]
    fn a_missing_kvm_device_fails() {
        let mut inputs = ready_inputs();
        inputs.distro.remove("kvm");
        inputs.distro.remove("kvm_open");
        let report = report(inputs);
        assert_eq!(check(&report, "kvm_device").status, Status::Fail);
        assert!(!report.ready);
    }

    #[test]
    fn wsl1_is_rejected() {
        let mut inputs = ready_inputs();
        inputs.wsl_version = Some("1.0.0.0".to_string());
        let report = report(inputs);
        assert_eq!(check(&report, "wsl_version").status, Status::Fail);
        assert!(!report.ready);
    }

    #[test]
    fn a_missing_wsl_reports_every_dependent_check() {
        let inputs = Inputs {
            wsl_version: None,
            wsl_kernel: None,
            default_distro: None,
            distro: BTreeMap::new(),
            ..ready_inputs()
        };
        let report = report(inputs);
        assert!(!report.ready);
        assert_eq!(check(&report, "wsl_version").status, Status::Fail);
        assert_eq!(check(&report, "wsl_distribution").status, Status::Fail);
        assert_eq!(check(&report, "nested_virtualization").status, Status::Fail);
    }

    #[test]
    fn the_human_report_matches_the_macos_helper_format() {
        let rendered = render_human(&report(ready_inputs()));
        assert!(rendered.starts_with("[PASS] wsl_version: WSL 2.7.14.0 is installed."));
        assert!(rendered.contains("[PASS] kvm_device: /dev/kvm opens inside WSL2"));
    }

    #[test]
    fn the_json_report_keeps_the_shared_field_names() {
        let json = serde_json::to_value(report(ready_inputs())).expect("report serializes");
        assert_eq!(json["platform"], "Windows");
        assert_eq!(json["product"], "microManager");
        assert_eq!(json["ready"], true);
        assert_eq!(
            json["validationGates"][1],
            "the management guest exposes usable /dev/kvm"
        );
        assert_eq!(json["checks"][0]["id"], "wsl_version");
    }
}
