//! `firecrab service doctor`: can this Windows host run the managed Debian guest?
//!
//! Check ids, statuses, and JSON field names match the macOS helper, so one
//! reader handles both hosts.

use std::collections::BTreeMap;

use serde::Serialize;

use super::wsl::{self, DISTRO_NAME};

const VALIDATION_GATES: [&str; 4] = [
    "architecture-matched Debian with systemd boots",
    "the management guest exposes usable /dev/kvm",
    "an actual Firecracker workload microVM boots",
    "guest networking, console, shutdown, and recovery pass",
];

/// Reports `key=value` lines so the parser never depends on guest locale.
///
/// `/dev/kvm` is opened only once it is a character device: as root, `<>` on a
/// missing path creates a regular file, and because every WSL2 distribution
/// shares one devtmpfs, that file then blocks the real device node for all of
/// them until `wsl --shutdown`.
const DISTRO_PROBE: &str = "printf 'kernel=%s\\n' \"$(uname -r)\"; \
printf 'machine=%s\\n' \"$(uname -m)\"; \
printf 'uptime=%s\\n' \"$(cut -d. -f1 /proc/uptime)\"; \
if [ -c /dev/kvm ]; then \
printf 'kvm=present\\n'; \
(exec 3<>/dev/kvm) 2>/dev/null && printf 'kvm_open=ok\\n'; \
elif [ -e /dev/kvm ]; then printf 'kvm_file=yes\\n'; fi; \
grep -Eqw 'vmx|svm' /proc/cpuinfo && printf 'nested=yes\\n'; \
exit 0";

/// `kvm_intel` loads about 25 seconds into the WSL2 utility VM's boot, so a probe
/// run right after a cold start sees no `/dev/kvm` on a host that supports it.
pub const KVM_SETTLE_SECONDS: u64 = 60;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warning,
    Fail,
}

impl Status {
    pub fn from_pass(passed: bool) -> Self {
        if passed { Status::Pass } else { Status::Fail }
    }

    /// The word every `firecrab service` command prints in brackets.
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warning => "WARNING",
            Status::Fail => "FAILED",
        }
    }
}

#[derive(Serialize, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub id: &'static str,
    pub status: Status,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub product: &'static str,
    pub platform: &'static str,
    pub os_version: String,
    pub architecture: &'static str,
    pub ready: bool,
    pub checks: Vec<Diagnostic>,
    pub validation_gates: [&'static str; 4],
}

pub struct Inputs {
    windows_version: String,
    /// The architecture this `firecrab.exe` was built for.
    build_architecture: &'static str,
    wsl_version: Option<String>,
    wsl_kernel: Option<String>,
    distributions: Vec<String>,
    /// `None` when no distribution exists yet, so nothing could be probed.
    probe: Option<BTreeMap<String, String>>,
}

impl Inputs {
    pub fn live() -> Self {
        let wsl = wsl::run(&["--version"]).unwrap_or_default();
        let distributions = wsl::distributions();
        // Root, because the managed guest runs everything as root; the user's
        // group membership in some other distribution says nothing about it.
        let probe = probe_target(&distributions).map(|target| {
            wsl::run(&[
                "-d",
                target,
                "-u",
                "root",
                "--exec",
                "sh",
                "-c",
                DISTRO_PROBE,
            ])
            .map(|text| parse_probe(&text))
            .unwrap_or_default()
        });
        Self {
            windows_version: wsl::run_program("cmd.exe", &["/c", "ver"])
                .ok()
                .as_deref()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            build_architecture: std::env::consts::ARCH,
            wsl_version: field(&wsl, "WSL version"),
            wsl_kernel: field(&wsl, "Kernel version"),
            distributions,
            probe,
        }
    }
}

/// The managed distribution once it exists; before install, the user's default.
/// Every WSL2 distribution shares one kernel, so either answers for `/dev/kvm`.
fn probe_target(distributions: &[String]) -> Option<&str> {
    if wsl::contains(distributions, DISTRO_NAME) {
        return Some(DISTRO_NAME);
    }
    distributions.first().map(String::as_str)
}

/// The value after `label:` in `wsl --version` style output.
pub(super) fn field(text: &str, label: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim() == label)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

/// The macOS helper reports `arm64`, so Windows does too.
fn report_architecture(architecture: &str) -> &'static str {
    match architecture {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        _ => "unknown",
    }
}

fn pass(id: &'static str, detail: String) -> Diagnostic {
    Diagnostic {
        id,
        status: Status::Pass,
        detail,
        fix: None,
    }
}

fn wsl_version_check(version: Option<&str>) -> Diagnostic {
    let Some(version) = version else {
        return Diagnostic {
            id: "wsl_version",
            status: Status::Fail,
            detail: "`wsl --version` did not report an installed WSL.".to_string(),
            fix: Some("Install WSL with `wsl --install --no-distribution`.".to_string()),
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
    pass("wsl_version", format!("WSL {version} is installed."))
}

fn architecture_check(build: &str, probe: Option<&BTreeMap<String, String>>) -> Diagnostic {
    if !matches!(build, "x86_64" | "aarch64") {
        return Diagnostic {
            id: "host_architecture",
            status: Status::Fail,
            detail: format!("microManager supports x86_64 and ARM64 hosts, not {build}."),
            fix: None,
        };
    }
    let machine = probe.and_then(|probe| probe.get("machine"));
    if let Some(machine) = machine.filter(|machine| machine.as_str() != build) {
        return Diagnostic {
            id: "host_architecture",
            status: Status::Fail,
            detail: format!("This {build} firecrab.exe runs on a {machine} WSL kernel."),
            fix: Some(format!(
                "Install the {} firecrab CLI with install-cli.ps1.",
                report_architecture(machine)
            )),
        };
    }
    pass(
        "host_architecture",
        format!("The host is {}.", report_architecture(build)),
    )
}

fn distribution_check(distributions: &[String]) -> Diagnostic {
    if wsl::contains(distributions, DISTRO_NAME) {
        return pass(
            "wsl_distribution",
            format!("The managed distribution {DISTRO_NAME} is imported."),
        );
    }
    let Some(default) = distributions.first() else {
        return Diagnostic {
            id: "wsl_distribution",
            status: Status::Warning,
            detail: "WSL has no distribution yet, so the guest checks could not run.".to_string(),
            fix: Some(format!(
                "`firecrab service install` imports {DISTRO_NAME} and checks it there."
            )),
        };
    };
    pass(
        "wsl_distribution",
        format!("{DISTRO_NAME} is not imported yet; checked {default} instead."),
    )
}

fn not_probed(id: &'static str) -> Diagnostic {
    Diagnostic {
        id,
        status: Status::Warning,
        detail: "Not checked: no WSL distribution is available to probe.".to_string(),
        fix: Some(format!(
            "`firecrab service install` checks this after importing {DISTRO_NAME}."
        )),
    }
}

fn kvm_check(probe: Option<&BTreeMap<String, String>>, kernel: &str) -> Diagnostic {
    let Some(probe) = probe else {
        return not_probed("kvm_device");
    };
    if probe.contains_key("kvm_file") {
        return Diagnostic {
            id: "kvm_device",
            status: Status::Fail,
            detail: "/dev/kvm is a regular file, so the KVM device node cannot appear.".to_string(),
            fix: Some("Run `wsl --shutdown`; WSL recreates /dev on its next start.".to_string()),
        };
    }
    if !probe.contains_key("kvm") {
        let uptime = probe.get("uptime").and_then(|value| value.parse().ok());
        if uptime.is_some_and(|seconds: u64| seconds < KVM_SETTLE_SECONDS) {
            return Diagnostic {
                id: "kvm_device",
                status: Status::Warning,
                detail: "/dev/kvm has not appeared yet; WSL is still starting.".to_string(),
                fix: Some("Wait for WSL to settle, then check again.".to_string()),
            };
        }
        return Diagnostic {
            id: "kvm_device",
            status: Status::Fail,
            detail: format!("/dev/kvm is missing from WSL2 kernel {kernel}."),
            fix: Some(
                "Set `nestedVirtualization=true` under [wsl2] in %UserProfile%\\.wslconfig, then run `wsl --shutdown`."
                    .to_string(),
            ),
        };
    }
    if !probe.contains_key("kvm_open") {
        return Diagnostic {
            id: "kvm_device",
            status: Status::Fail,
            detail: "/dev/kvm exists but root cannot open it.".to_string(),
            fix: Some("Run `wsl --shutdown` and check again.".to_string()),
        };
    }
    pass(
        "kvm_device",
        format!("/dev/kvm opens inside WSL2 on kernel {kernel}."),
    )
}

fn nested_virtualization_check(probe: Option<&BTreeMap<String, String>>) -> Diagnostic {
    let Some(probe) = probe else {
        return not_probed("nested_virtualization");
    };
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
    pass(
        "nested_virtualization",
        "The WSL2 guest sees hardware virtualization extensions.".to_string(),
    )
}

pub fn report(inputs: Inputs) -> CapabilityReport {
    let probe = inputs.probe.as_ref();
    let kernel = probe
        .and_then(|probe| probe.get("kernel"))
        .or(inputs.wsl_kernel.as_ref())
        .map(String::as_str)
        .unwrap_or("unknown");
    let checks = vec![
        wsl_version_check(inputs.wsl_version.as_deref()),
        architecture_check(inputs.build_architecture, probe),
        distribution_check(&inputs.distributions),
        kvm_check(probe, kernel),
        nested_virtualization_check(probe),
    ];
    CapabilityReport {
        product: "microManager",
        platform: "Windows",
        ready: !checks.iter().any(|check| check.status == Status::Fail),
        checks,
        os_version: inputs.windows_version,
        architecture: report_architecture(inputs.build_architecture),
        validation_gates: VALIDATION_GATES,
    }
}

pub fn render_human(report: &CapabilityReport) -> String {
    report
        .checks
        .iter()
        .map(|check| format!("[{}] {}: {}", check.status.label(), check.id, check.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_inputs() -> Inputs {
        Inputs {
            windows_version: "Microsoft Windows [Version 10.0.26200.1]".to_string(),
            build_architecture: "x86_64",
            wsl_version: Some("2.7.14.0".to_string()),
            wsl_kernel: Some("6.18.33.2-2".to_string()),
            distributions: wsl::parse_list("Debian\r\nfirecrab-debian\r\n"),
            probe: Some(parse_probe(
                "kernel=6.18.33.2-microsoft-standard-WSL2\nmachine=x86_64\nuptime=900\nkvm=present\nkvm_open=ok\nnested=yes\n",
            )),
        }
    }

    fn check<'a>(report: &'a CapabilityReport, id: &str) -> &'a Diagnostic {
        report
            .checks
            .iter()
            .find(|check| check.id == id)
            .expect("check is reported")
    }

    fn probe_mut(inputs: &mut Inputs) -> &mut BTreeMap<String, String> {
        inputs.probe.as_mut().expect("probe ran")
    }

    #[test]
    fn live_inputs_probe_the_managed_distribution_as_root() {
        let wsl = wsl::fake::answer(|line| {
            match line {
            "wsl.exe --version" => Ok("WSL version: 2.7.14.0\nKernel version: 6.18.33.2-2\n".into()),
            "wsl.exe --list --quiet" => Ok("Debian\nfirecrab-debian\n".into()),
            "cmd.exe /c ver" => Ok("\nMicrosoft Windows [Version 10.0.26200.1]\n".into()),
            l if l.starts_with("wsl.exe -d firecrab-debian -u root --exec sh -c ") => {
                Ok("kernel=6.18.33.2-microsoft-standard-WSL2\nmachine=x86_64\nuptime=900\nkvm=present\nkvm_open=ok\nnested=yes\n".into())
            }
            other => panic!("unexpected {other}"),
        }
        });
        let inputs = Inputs::live();
        assert_eq!(
            inputs.windows_version,
            "Microsoft Windows [Version 10.0.26200.1]"
        );
        assert_eq!(inputs.wsl_version.as_deref(), Some("2.7.14.0"));
        assert_eq!(inputs.distributions, ["Debian", "firecrab-debian"]);
        assert_eq!(
            inputs
                .probe
                .as_ref()
                .and_then(|probe| probe.get("kvm_open"))
                .map(String::as_str),
            Some("ok")
        );
        assert_eq!(wsl.calls().len(), 4);
    }

    #[test]
    fn a_host_without_wsl_is_not_probed() {
        let _wsl = wsl::fake::answer(|_| Err("not found".into()));
        let inputs = Inputs::live();
        assert_eq!(inputs.windows_version, "unknown");
        assert!(inputs.probe.is_none());
        assert!(!report(inputs).ready);
    }

    #[test]
    fn reads_wsl_version_fields() {
        let text = "WSL version: 2.7.14.0\nKernel version: 6.18.33.2-2\n";
        assert_eq!(field(text, "WSL version").as_deref(), Some("2.7.14.0"));
        assert_eq!(
            field(text, "Kernel version").as_deref(),
            Some("6.18.33.2-2")
        );
        assert_eq!(field(text, "WSLg version"), None);
    }

    #[test]
    fn the_managed_distribution_is_probed_once_it_exists() {
        let listed = wsl::parse_list("Debian\nfirecrab-debian\n");
        assert_eq!(probe_target(&listed), Some(DISTRO_NAME));
        let fresh = wsl::parse_list("Ubuntu\nDebian\n");
        assert_eq!(probe_target(&fresh), Some("Ubuntu"));
        assert_eq!(probe_target(&[]), None);
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
    fn a_host_without_any_distribution_can_still_install() {
        let report = report(Inputs {
            distributions: Vec::new(),
            probe: None,
            ..ready_inputs()
        });
        assert!(report.ready, "install imports its own distribution");
        for id in ["wsl_distribution", "kvm_device", "nested_virtualization"] {
            assert_eq!(check(&report, id).status, Status::Warning, "{id}");
        }
    }

    #[test]
    fn before_install_the_default_distribution_answers() {
        let report = report(Inputs {
            distributions: wsl::parse_list("Ubuntu\n"),
            ..ready_inputs()
        });
        let distribution = check(&report, "wsl_distribution");
        assert_eq!(distribution.status, Status::Pass);
        assert!(distribution.detail.contains("checked Ubuntu instead"));
    }

    #[test]
    fn an_unopenable_kvm_device_fails() {
        let mut inputs = ready_inputs();
        probe_mut(&mut inputs).remove("kvm_open");
        let report = report(inputs);
        assert_eq!(check(&report, "kvm_device").status, Status::Fail);
        assert!(!report.ready);
    }

    #[test]
    fn a_cold_started_wsl_warns_instead_of_failing() {
        let mut inputs = ready_inputs();
        let probe = probe_mut(&mut inputs);
        probe.remove("kvm");
        probe.remove("kvm_open");
        probe.insert("uptime".to_string(), "12".to_string());
        let report = report(inputs);
        assert_eq!(check(&report, "kvm_device").status, Status::Warning);
        assert!(report.ready, "a still-starting WSL must not block install");
    }

    #[test]
    fn the_probe_never_opens_a_path_that_is_not_the_device() {
        let open = DISTRO_PROBE
            .find("3<>/dev/kvm")
            .expect("probe opens the device");
        let guard = DISTRO_PROBE
            .find("if [ -c /dev/kvm ]")
            .expect("probe checks the type");
        assert!(
            guard < open,
            "the open must sit behind the character-device check"
        );
        assert!(!DISTRO_PROBE.contains("[ -e /dev/kvm ] && printf 'kvm=present"));
    }

    #[test]
    fn a_regular_file_in_place_of_the_device_names_the_fix() {
        let mut inputs = ready_inputs();
        let probe = probe_mut(&mut inputs);
        probe.remove("kvm");
        probe.remove("kvm_open");
        probe.insert("kvm_file".to_string(), "yes".to_string());
        let report = report(inputs);
        let kvm = check(&report, "kvm_device");
        assert_eq!(kvm.status, Status::Fail);
        assert!(
            kvm.fix
                .as_deref()
                .is_some_and(|fix| fix.contains("wsl --shutdown"))
        );
    }

    #[test]
    fn a_missing_kvm_device_fails() {
        let mut inputs = ready_inputs();
        let probe = probe_mut(&mut inputs);
        probe.remove("kvm");
        probe.remove("kvm_open");
        let report = report(inputs);
        assert_eq!(check(&report, "kvm_device").status, Status::Fail);
        assert!(!report.ready);
    }

    #[test]
    fn missing_virtualization_extensions_fail() {
        let mut inputs = ready_inputs();
        probe_mut(&mut inputs).remove("nested");
        let report = report(inputs);
        assert_eq!(check(&report, "nested_virtualization").status, Status::Fail);
    }

    #[test]
    fn wsl1_and_missing_wsl_are_rejected() {
        let old = report(Inputs {
            wsl_version: Some("1.0.0.0".to_string()),
            ..ready_inputs()
        });
        assert_eq!(check(&old, "wsl_version").status, Status::Fail);
        let missing = report(Inputs {
            wsl_version: None,
            ..ready_inputs()
        });
        assert_eq!(check(&missing, "wsl_version").status, Status::Fail);
        assert!(!missing.ready);
    }

    #[test]
    fn arm64_hosts_are_supported() {
        let mut inputs = Inputs {
            build_architecture: "aarch64",
            ..ready_inputs()
        };
        probe_mut(&mut inputs).insert("machine".to_string(), "aarch64".to_string());
        let report = report(inputs);
        assert!(report.ready);
        assert_eq!(report.architecture, "arm64");
        assert_eq!(
            check(&report, "host_architecture").detail,
            "The host is arm64."
        );
    }

    #[test]
    fn an_emulated_cli_is_told_to_install_the_native_one() {
        let mut inputs = ready_inputs();
        probe_mut(&mut inputs).insert("machine".to_string(), "aarch64".to_string());
        let report = report(inputs);
        let architecture = check(&report, "host_architecture");
        assert_eq!(architecture.status, Status::Fail);
        assert!(
            architecture
                .fix
                .as_deref()
                .is_some_and(|fix| fix.contains("arm64"))
        );
    }

    #[test]
    fn unsupported_builds_fail() {
        let report = report(Inputs {
            build_architecture: "x86",
            ..ready_inputs()
        });
        assert_eq!(check(&report, "host_architecture").status, Status::Fail);
        assert_eq!(report.architecture, "unknown");
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
        assert_eq!(json["checks"][1]["id"], "host_architecture");
    }
}
