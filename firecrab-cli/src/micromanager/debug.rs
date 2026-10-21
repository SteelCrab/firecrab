//! A read-only snapshot of the managed host. The platform modules supply live
//! probes; this module reads only the small runtime markers and bounded logs.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const MAX_MARKER_BYTES: u64 = 4096;

#[derive(Clone, Copy)]
pub struct Options {
    pub json: bool,
    pub logs: bool,
    pub tail: usize,
}

impl Options {
    pub fn new(json: bool, logs: bool, tail: Option<u16>) -> Self {
        Self {
            json,
            logs,
            tail: usize::from(tail.unwrap_or(200)),
        }
    }
}

#[derive(Serialize)]
pub struct Probe {
    pub state: &'static str,
    pub detail: String,
}

impl Probe {
    pub fn pass(detail: impl Into<String>) -> Self {
        Self {
            state: "pass",
            detail: redact(&detail.into()),
        }
    }

    pub fn fail(detail: impl Into<String>) -> Self {
        Self {
            state: "failed",
            detail: redact(&detail.into()),
        }
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            state: "unavailable",
            detail: redact(&detail.into()),
        }
    }
}

#[derive(Default, Serialize)]
pub struct Provision {
    pub complete: bool,
    pub phase: Option<String>,
    pub failure: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Deserialize, Serialize)]
pub struct CapabilityCheck {
    pub id: String,
    pub status: String,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Serialize)]
pub struct Capability {
    #[serde(flatten)]
    pub probe: Probe,
    pub checks: Vec<CapabilityCheck>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Deserialize)]
struct DoctorSummary {
    ready: bool,
    checks: Vec<CapabilityCheck>,
}

impl Capability {
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            probe: Probe::unavailable(detail),
            checks: Vec::new(),
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let summary: DoctorSummary = serde_json::from_str(text)?;
        Ok(Self::from_checks(summary.ready, summary.checks))
    }

    pub fn from_checks(ready: bool, checks: Vec<CapabilityCheck>) -> Self {
        let checks: Vec<_> = checks
            .into_iter()
            .map(|check| CapabilityCheck {
                id: check.id,
                status: check.status,
                detail: redact(&check.detail),
                fix: check.fix.map(|fix| redact(&fix)),
            })
            .collect();
        let failed = checks.iter().filter(|check| check.status == "fail").count();
        let warnings = checks
            .iter()
            .filter(|check| check.status == "warning")
            .count();
        let probe = if failed > 0 || !ready {
            Probe::fail(format!("{failed} host capability checks failed"))
        } else if warnings > 0 {
            Probe::unavailable(format!("{warnings} host capability checks could not run"))
        } else {
            Probe::pass("host capability checks passed")
        };
        Self { probe, checks }
    }
}

#[derive(Serialize)]
pub struct LogSource {
    pub name: &'static str,
    pub location: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl LogSource {
    pub fn guest(location: impl Into<String>, result: Result<String, String>, tail: usize) -> Self {
        match result {
            Ok(excerpt) => Self {
                name: "guest-journal",
                location: location.into(),
                state: "present",
                excerpt: Some(redact(&tail_text(&excerpt, tail))),
                error: None,
            },
            Err(error) => Self {
                name: "guest-journal",
                location: location.into(),
                state: "unavailable",
                excerpt: None,
                error: Some(redact(&tail_text(&error, 20))),
            },
        }
    }
}

#[derive(Serialize)]
pub struct DebugReport {
    pub platform: &'static str,
    pub managed_home: String,
    pub capability: Capability,
    pub service: Probe,
    pub guest: Probe,
    pub api: Probe,
    pub provision: Provision,
    pub logs: Vec<LogSource>,
}

impl DebugReport {
    pub fn new(
        platform: &'static str,
        managed_home: &Path,
        options: Options,
        local_logs: &[(&'static str, &'static str)],
    ) -> Self {
        let runtime = managed_home.join("runtime");
        Self {
            platform,
            managed_home: managed_home.display().to_string(),
            capability: Capability::unavailable("not checked"),
            service: Probe::unavailable("not checked"),
            guest: Probe::unavailable("not checked"),
            api: Probe::unavailable("not checked"),
            provision: read_provision(&runtime),
            logs: local_logs
                .iter()
                .map(|&(name, file)| local_log(name, runtime.join(file), options))
                .collect(),
        }
    }

    pub fn render_human(&self) -> String {
        let mut lines = vec![
            format!("microManager debug ({})", self.platform),
            format!("  managed home: {}", self.managed_home),
            format_probe("capability", &self.capability.probe),
        ];
        for check in self
            .capability
            .checks
            .iter()
            .filter(|check| check.status != "pass")
        {
            lines.push(format!("  {}: {}", check.id, check.detail));
            if let Some(fix) = &check.fix {
                lines.push(format!("    fix: {fix}"));
            }
        }
        lines.extend([
            format_probe("service", &self.service),
            format_probe("guest", &self.guest),
            format_probe("api", &self.api),
            format!(
                "[{}] provision: {}",
                if self.provision.failure.is_some() {
                    "FAILED"
                } else if self.provision.complete {
                    "PASS"
                } else {
                    "WARNING"
                },
                self.provision
                    .phase
                    .as_deref()
                    .unwrap_or("no phase recorded")
            ),
        ]);
        if let Some(failure) = &self.provision.failure {
            lines.push(format!("  failure: {failure}"));
        }
        for error in &self.provision.errors {
            lines.push(format!("  marker error: {error}"));
        }
        lines.push("  logs:".to_string());
        for log in &self.logs {
            lines.push(format!(
                "    {}: {} ({})",
                log.name, log.location, log.state
            ));
            if let Some(error) = &log.error {
                lines.push(format!("      {error}"));
            }
            if let Some(excerpt) = &log.excerpt {
                lines.push(format!("      --- {} ---", log.name));
                lines.extend(excerpt.lines().map(|line| format!("      {line}")));
            }
        }
        lines.join("\n")
    }
}

fn format_probe(name: &str, probe: &Probe) -> String {
    format!(
        "[{}] {name}: {}",
        probe.state.to_ascii_uppercase(),
        probe.detail
    )
}

fn read_provision(runtime: &Path) -> Provision {
    let mut provision = Provision {
        complete: runtime.join("provisioned").is_file(),
        ..Provision::default()
    };
    for (name, destination) in [
        ("provision.phase", &mut provision.phase),
        ("provision.failed", &mut provision.failure),
    ] {
        match read_small_file(&runtime.join(name)) {
            Ok(value) => *destination = value.map(|text| redact(text.trim())),
            Err(error) => provision.errors.push(format!("{name}: {error}")),
        }
    }
    provision
}

fn read_small_file(path: &Path) -> io::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file",
            ));
        }
        Ok(_) => {}
    }
    let mut text = String::new();
    File::open(path)?
        .take(MAX_MARKER_BYTES)
        .read_to_string(&mut text)?;
    Ok(Some(text))
}

fn local_log(name: &'static str, path: PathBuf, options: Options) -> LogSource {
    let mut source = LogSource {
        name,
        location: path.display().to_string(),
        state: "missing",
        excerpt: None,
        error: None,
    };
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return source,
        Err(error) => {
            source.state = "unavailable";
            source.error = Some(error.to_string());
            return source;
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            source.state = "unavailable";
            source.error = Some("not a regular file".to_string());
            return source;
        }
        Ok(_) => source.state = "present",
    }
    if options.logs {
        match tail_file(&path, options.tail) {
            Ok(excerpt) => source.excerpt = Some(redact(&excerpt)),
            Err(error) => {
                source.state = "unavailable";
                source.error = Some(error.to_string());
            }
        }
    }
    source
}

fn tail_file(path: &Path, count: usize) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(MAX_LOG_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let mut lines: Vec<_> = text.lines().rev().take(count).collect();
    lines.reverse();
    Ok(lines.join("\n"))
}

fn tail_text(text: &str, count: usize) -> String {
    let mut start = text.len().saturating_sub(MAX_LOG_BYTES as usize);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let text = if start > 0 {
        text[start..].split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        text
    };
    let mut lines: Vec<_> = text.lines().rev().take(count).collect();
    lines.reverse();
    lines.join("\n")
}

fn redact(text: &str) -> String {
    let mut key_block = false;
    text.lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.contains("-----begin ") && lower.contains("private key") {
                key_block = true;
            }
            let sensitive = key_block
                || [
                    "authorization:",
                    "bearer ",
                    "password=",
                    "passwd=",
                    "token=",
                    "api_key=",
                    "secret=",
                    "private_key=",
                    "cookie:",
                    "set-cookie:",
                ]
                .iter()
                .any(|marker| lower.contains(marker));
            if lower.contains("-----end ") && lower.contains("private key") {
                key_block = false;
            }
            if sensitive { "[REDACTED]" } else { line }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_handle_missing_sources_and_bound_log_output() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::write(runtime.join("provision.phase"), "install-packages\n").unwrap();
        fs::write(
            runtime.join("guest-provision.log"),
            "first\nAuthorization: Bearer abc\nthird\nfourth\n",
        )
        .unwrap();
        let report = DebugReport::new(
            "windows",
            directory.path(),
            Options::new(false, true, Some(2)),
            &[
                ("provision", "guest-provision.log"),
                ("daemon", "daemon.log"),
            ],
        );
        assert_eq!(report.provision.phase.as_deref(), Some("install-packages"));
        assert_eq!(report.logs[0].excerpt.as_deref(), Some("third\nfourth"));
        assert_eq!(report.logs[1].state, "missing");
        assert!(!report.render_human().contains("first"));
    }

    #[test]
    fn redaction_hides_credentials_and_private_key_blocks() {
        let text = "before\ntoken=abc\n-----BEGIN OPENSSH PRIVATE KEY-----\nsecret material\n-----END OPENSSH PRIVATE KEY-----\nafter";
        assert_eq!(
            redact(text),
            "before\n[REDACTED]\n[REDACTED]\n[REDACTED]\n[REDACTED]\nafter"
        );
    }

    #[test]
    fn guest_journal_is_bounded_and_redacted() {
        let source = LogSource::guest(
            "journalctl",
            Ok("first\nAuthorization: Bearer abc\nthird".to_string()),
            2,
        );
        assert_eq!(source.excerpt.as_deref(), Some("[REDACTED]\nthird"));
    }

    #[test]
    fn capability_json_keeps_failed_checks_and_fixes() {
        let report = Capability::from_json(
            r#"{"ready":false,"checks":[{"id":"nested_virtualization","status":"fail","detail":"Unavailable","fix":"Use a supported host"}]}"#,
        )
        .unwrap();
        assert_eq!(report.probe.state, "failed");
        assert_eq!(report.checks[0].id, "nested_virtualization");
        assert_eq!(
            report.checks[0].fix.as_deref(),
            Some("Use a supported host")
        );
    }

    #[cfg(unix)]
    #[test]
    fn diagnostics_do_not_follow_log_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        let secret = directory.path().join("secret");
        fs::write(&secret, "password=abc\n").unwrap();
        symlink(secret, runtime.join("daemon.log")).unwrap();
        let report = DebugReport::new(
            "macos",
            directory.path(),
            Options::new(false, true, None),
            &[("daemon", "daemon.log")],
        );
        assert_eq!(report.logs[0].state, "unavailable");
        assert!(report.logs[0].excerpt.is_none());
    }
}
