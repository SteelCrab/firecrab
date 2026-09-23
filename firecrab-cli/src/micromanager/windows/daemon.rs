//! The resident management VM on Windows.
//!
//! WSL stops a distribution about 15 seconds after its last client exits, even
//! with systemd running inside, so something has to hold it open. A per-user
//! scheduled task runs `wslg.exe -- sleep infinity`: no console window, no
//! administrator rights. The task starts at logon and re-fires every minute,
//! and `IgnoreNew` makes that a no-op while the holder runs. Together they play
//! the part of launchd's `RunAtLoad` and `KeepAlive` on macOS.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use firecrab_api_types::HostOs;

use super::super::host_platform::{self, HOST_PLATFORM_DESCRIPTOR_PATH};
use super::doctor;
use super::lifecycle::Layout;
use super::wsl::{self, DISTRO_NAME};

pub const TASK_NAME: &str = r"Firecrab\microManager";
pub const API_URL: &str = "http://127.0.0.1:5523/api/host";
const UNITS: [&str; 2] = ["firecrab-api", "firecrab-net-helper"];
/// A cold start waits for `kvm_intel`, systemd, and the API in turn.
const READY_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Wsl(#[from] wsl::Error),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("USERNAME is not set; the scheduled task needs the current user")]
    MissingUser,
    #[error("{0} is missing; update WSL with `wsl --update`")]
    MissingLauncher(PathBuf),
    #[error("microManager is not installed; run `firecrab service install`")]
    NotInstalled,
    #[error("the management VM did not become ready within {0} seconds")]
    ReadyTimeout(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Missing,
    Disabled,
    Enabled,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub task: TaskState,
    pub ready: bool,
    pub api_reachable: bool,
    pub detail: Option<String>,
}

impl Status {
    pub fn success(&self) -> bool {
        self.task == TaskState::Enabled && self.ready && self.api_reachable
    }
}

/// Registers (or replaces) the task. It is written to `runtime/` first because
/// `schtasks /Create /XML` only reads definitions from a file.
pub fn install(layout: &Layout) -> Result<PathBuf, Error> {
    let user = user_id(
        std::env::var("USERNAME").ok(),
        std::env::var("USERDOMAIN").ok(),
    )?;
    let launcher = launcher_in(std::env::var_os("ProgramFiles").map(PathBuf::from));
    if !launcher.is_file() {
        return Err(Error::MissingLauncher(launcher));
    }
    register(layout, &render_task(&user, &launcher))
}

fn register(layout: &Layout, task: &str) -> Result<PathBuf, Error> {
    let definition = layout.runtime().join("microManager-task.xml");
    fs::write(&definition, utf16_with_bom(task)).map_err(|source| Error::Io {
        action: "write the scheduled task definition",
        path: definition.clone(),
        source,
    })?;
    schtasks(&[
        "/Create",
        "/TN",
        TASK_NAME,
        "/XML",
        &definition.to_string_lossy(),
        "/F",
    ])?;
    Ok(definition)
}

pub fn uninstall(layout: &Layout) -> Result<(), Error> {
    stop()?;
    if task_state() != TaskState::Missing {
        schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])?;
    }
    let definition = layout.runtime().join("microManager-task.xml");
    match fs::remove_file(&definition) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            action: "remove the scheduled task definition",
            path: definition,
            source,
        }),
    }
}

pub fn start() -> Result<Status, Error> {
    if task_state() == TaskState::Missing {
        return Err(Error::NotInstalled);
    }
    publish_host_platform();
    schtasks(&["/Change", "/TN", TASK_NAME, "/ENABLE"])?;
    schtasks(&["/Run", "/TN", TASK_NAME])?;
    wait_ready(READY_TIMEOUT)
}

/// Leaves the Windows and WSL versions where the guest's API reports them.
/// Refreshed on every start, since Windows and WSL updates change both; a
/// failure only costs the dashboard's Host view, so it never stops a start.
fn publish_host_platform() {
    let windows = wsl::run_program("cmd.exe", &["/c", "ver"]).unwrap_or_default();
    let wsl_version = wsl::run(&["--version"])
        .ok()
        .and_then(|text| doctor::field(&text, "WSL version"));
    let (name, version) = windows_release(&windows);
    let virtualization = match wsl_version {
        Some(version) => format!("WSL2 {version}"),
        None => "WSL2".to_string(),
    };
    let descriptor =
        host_platform::descriptor_json(HostOs::Windows, name, &version, &virtualization);
    let script = format!(
        "mkdir -p /etc/firecrab && printf '%s\\n' {} > {HOST_PLATFORM_DESCRIPTOR_PATH}",
        wsl::shell_quote(&descriptor)
    );
    if let Err(error) = wsl::root_shell(&script) {
        println!("[WARNING] host_platform: {error}");
    }
}

/// `Microsoft Windows [Version 10.0.26200.9457]` becomes `Windows 11` and
/// `10.0.26200.9457`. The word "Version" is localized, so the dotted number is
/// found by shape. Windows 11 kept version 10.0 and starts at build 22000.
fn windows_release(ver: &str) -> (&'static str, String) {
    let Some(version) = ver
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .find(|word| {
            word.split('.').count() >= 3
                && word
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
    else {
        return ("Windows", String::new());
    };
    let build: u32 = version
        .split('.')
        .nth(2)
        .and_then(|build| build.parse().ok())
        .unwrap_or(0);
    let name = if build >= 22000 {
        "Windows 11"
    } else {
        "Windows 10"
    };
    (name, version.to_string())
}

/// Disables the task first, or its one-minute trigger would start the guest again.
pub fn stop() -> Result<(), Error> {
    if task_state() != TaskState::Missing {
        schtasks(&["/Change", "/TN", TASK_NAME, "/DISABLE"])?;
        // Fails when the holder is not running, which is the state we want.
        let _ = schtasks(&["/End", "/TN", TASK_NAME]);
    }
    if wsl::is_running(DISTRO_NAME) {
        // Let the services stop their microVMs before the distribution goes away.
        let _ = wsl::root_shell(&format!("systemctl stop {}", UNITS.join(" ")));
        wsl::run(&["--terminate", DISTRO_NAME])?;
    }
    Ok(())
}

pub fn status() -> Status {
    let task = task_state();
    let (ready, detail) = if wsl::is_running(DISTRO_NAME) {
        guest_units()
    } else {
        (false, Some("not running".to_string()))
    };
    Status {
        task,
        ready,
        api_reachable: api_reachable(),
        detail,
    }
}

fn wait_ready(timeout: Duration) -> Result<Status, Error> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let status = status();
        if status.success() || status.task != TaskState::Enabled {
            return Ok(status);
        }
        thread::sleep(Duration::from_secs(2));
    }
    Err(Error::ReadyTimeout(timeout.as_secs()))
}

pub fn task_state() -> TaskState {
    match schtasks(&["/Query", "/TN", TASK_NAME, "/XML"]) {
        Ok(xml) if settings_enabled(&xml) => TaskState::Enabled,
        Ok(_) => TaskState::Disabled,
        Err(_) => TaskState::Missing,
    }
}

/// `schtasks /Query` prints localized text, but the XML form is not localized.
/// Only the `<Settings>` block's `<Enabled>` says whether the task may run;
/// each trigger carries its own.
fn settings_enabled(xml: &str) -> bool {
    let settings = xml
        .split_once("<Settings>")
        .and_then(|(_, rest)| rest.split_once("</Settings>"))
        .map(|(settings, _)| settings)
        .unwrap_or_default();
    !settings.contains("<Enabled>false</Enabled>")
}

/// Reports each unit as `name=state` so an inactive unit is not a failed command.
fn guest_units() -> (bool, Option<String>) {
    let script = format!(
        "for unit in {}; do printf '%s=%s\\n' \"$unit\" \"$(systemctl is-active \"$unit\")\"; done; \
         printf 'ip=%s\\n' \"$(hostname -I | cut -d' ' -f1)\"",
        UNITS.join(" ")
    );
    match wsl::root_shell(&script) {
        Ok(text) => parse_units(&text),
        Err(error) => (false, Some(error.to_string())),
    }
}

fn parse_units(text: &str) -> (bool, Option<String>) {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let ready = UNITS
        .iter()
        .all(|unit| lines.contains(&format!("{unit}=active").as_str()));
    (ready, Some(lines.join(", ")))
}

fn api_reachable() -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(API_URL).send())
        .is_ok_and(|response| response.status().is_success())
}

fn schtasks(args: &[&str]) -> Result<String, wsl::Error> {
    wsl::run_program("schtasks.exe", args)
}

/// `DOMAIN\user`, the form a task principal and logon trigger expect.
fn user_id(username: Option<String>, domain: Option<String>) -> Result<String, Error> {
    let user = username
        .filter(|user| !user.is_empty())
        .ok_or(Error::MissingUser)?;
    Ok(match domain.filter(|domain| !domain.is_empty()) {
        Some(domain) => format!("{domain}\\{user}"),
        None => user,
    })
}

/// `wslg.exe` is the windowless twin of `wsl.exe` that ships with WSL itself.
/// It does not accept `--exec`, so the holder runs through `--`.
fn launcher_in(program_files: Option<PathBuf>) -> PathBuf {
    program_files
        .unwrap_or_else(|| PathBuf::from("C:\\Program Files"))
        .join("WSL")
        .join("wslg.exe")
}

fn render_task(user: &str, launcher: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Keeps the Firecrab management distribution {distro} running.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
    <TimeTrigger>
      <Enabled>true</Enabled>
      <StartBoundary>2000-01-01T00:00:00</StartBoundary>
      <Repetition>
        <Interval>PT1M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
    <Hidden>true</Hidden>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{launcher}</Command>
      <Arguments>-d {distro} -u root -- sleep infinity</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        distro = DISTRO_NAME,
        user = xml_escape(user),
        launcher = xml_escape(&launcher.to_string_lossy()),
    )
}

/// The XML declares UTF-16, so the bytes on disk must match it.
fn utf16_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_holds_the_managed_distribution_open() {
        let task = render_task(
            "LAB\\dev & co",
            Path::new("C:\\Program Files\\WSL\\wslg.exe"),
        );
        assert!(task.contains("<UserId>LAB\\dev &amp; co</UserId>"));
        assert!(task.contains("<Command>C:\\Program Files\\WSL\\wslg.exe</Command>"));
        assert!(
            task.contains("<Arguments>-d firecrab-debian -u root -- sleep infinity</Arguments>")
        );
        assert!(!task.contains("--exec"), "wslg.exe rejects --exec");
        assert!(task.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
        assert!(task.contains("<Interval>PT1M</Interval>"));
        assert!(task.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(task.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }

    use super::super::wsl::fake;

    const ENABLED: &str = "<Task><Settings><Enabled>true</Enabled></Settings></Task>";
    const DISABLED: &str = "<Task><Settings><Enabled>false</Enabled></Settings></Task>";

    fn layout() -> (tempfile::TempDir, Layout) {
        let directory = tempfile::tempdir().expect("temp dir");
        let layout = Layout {
            managed_home: directory.path().join("micromanager"),
        };
        super::super::lifecycle::prepare(&layout).expect("managed directories");
        (directory, layout)
    }

    #[test]
    fn the_task_principal_is_domain_qualified_when_known() {
        assert_eq!(
            user_id(Some("dev".into()), Some("LAB".into())).expect("user"),
            "LAB\\dev"
        );
        assert_eq!(
            user_id(Some("dev".into()), Some(String::new())).expect("user"),
            "dev"
        );
        assert!(matches!(user_id(None, None), Err(Error::MissingUser)));
    }

    #[test]
    fn the_launcher_lives_under_program_files() {
        assert_eq!(
            launcher_in(Some(PathBuf::from("D:\\Apps"))),
            PathBuf::from("D:\\Apps").join("WSL").join("wslg.exe")
        );
        assert!(launcher_in(None).ends_with("wslg.exe"));
    }

    #[test]
    fn registering_writes_the_definition_and_hands_it_to_schtasks() {
        let (_directory, layout) = layout();
        let schtasks = fake::answer(|_| Ok("SUCCESS".into()));
        let definition = register(&layout, "<Task/>").expect("registered");
        assert_eq!(
            fs::read(&definition).expect("written"),
            utf16_with_bom("<Task/>")
        );
        assert_eq!(
            schtasks.calls(),
            [format!(
                "schtasks.exe /Create /TN Firecrab\\microManager /XML {} /F",
                definition.display()
            )]
        );
    }

    #[test]
    fn windows_releases_are_named_by_build() {
        assert_eq!(
            windows_release("\r\nMicrosoft Windows [Version 10.0.26200.9457]\r\n"),
            ("Windows 11", "10.0.26200.9457".to_string())
        );
        assert_eq!(
            windows_release("Microsoft Windows [Versión 10.0.19045.4529]"),
            ("Windows 10", "10.0.19045.4529".to_string())
        );
        assert_eq!(windows_release(""), ("Windows", String::new()));
    }

    #[test]
    fn start_publishes_the_host_platform_into_the_guest() {
        let wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(DISABLED.into()),
            "cmd.exe /c ver" => Ok("Microsoft Windows [Version 10.0.26200.9457]\n".into()),
            "wsl.exe --version" => {
                Ok("WSL version: 2.7.14.0\nKernel version: 6.18.33.2-2\n".into())
            }
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            _ => Ok(String::new()),
        });
        start().expect("start returns once the task reads back disabled");
        let write = wsl
            .calls()
            .into_iter()
            .find(|call| call.contains("/etc/firecrab/host-platform.json"))
            .expect("the descriptor is written");
        assert!(
            write.starts_with(
                "wsl.exe -d firecrab-debian -u root --exec sh -c mkdir -p /etc/firecrab"
            )
        );
        assert!(write.contains(r#""name":"Windows 11""#));
        assert!(write.contains(r#""version":"10.0.26200.9457""#));
        assert!(write.contains(r#""virtualization":"WSL2 2.7.14.0""#));
    }

    #[test]
    fn a_failed_platform_write_does_not_stop_the_start() {
        let schtasks = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(DISABLED.into()),
            l if l.contains("host-platform.json") => Err("Catastrophic failure".into()),
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            _ => Ok(String::new()),
        });
        start().expect("the Host view is cosmetic");
        assert!(
            schtasks
                .calls()
                .iter()
                .any(|call| call.ends_with("/Run /TN Firecrab\\microManager"))
        );
    }

    #[test]
    fn task_state_reads_registration_and_the_enabled_flag() {
        let _missing =
            fake::answer(|_| Err("ERROR: The system cannot find the file specified.".into()));
        assert_eq!(task_state(), TaskState::Missing);
        let _disabled = fake::answer(|_| Ok(DISABLED.into()));
        assert_eq!(task_state(), TaskState::Disabled);
        let _enabled = fake::answer(|_| Ok(ENABLED.into()));
        assert_eq!(task_state(), TaskState::Enabled);
    }

    #[test]
    fn start_refuses_an_uninstalled_host() {
        let _schtasks = fake::answer(|_| Err("not found".into()));
        assert!(matches!(start(), Err(Error::NotInstalled)));
    }

    #[test]
    fn start_enables_runs_and_stops_waiting_once_the_task_is_disabled() {
        let mut queries = 0;
        let schtasks = fake::answer(move |line| {
            if line.starts_with("schtasks.exe /Query") {
                queries += 1;
                // Registered for the pre-check, then disabled under us mid-wait.
                return Ok(if queries == 1 { ENABLED } else { DISABLED }.into());
            }
            if line.starts_with("wsl.exe --list --running") {
                return Ok(String::new());
            }
            Ok("SUCCESS".into())
        });
        let status = start().expect("start returns a status");
        assert_eq!(status.task, TaskState::Disabled);
        assert!(!status.success());
        let calls = schtasks.calls();
        assert!(
            calls.contains(&"schtasks.exe /Change /TN Firecrab\\microManager /ENABLE".to_string())
        );
        assert!(calls.contains(&"schtasks.exe /Run /TN Firecrab\\microManager".to_string()));
    }

    #[test]
    fn stop_disables_before_ending_and_terminates_a_running_guest() {
        let wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            "schtasks.exe /End /TN Firecrab\\microManager" => Err("not running".into()),
            "wsl.exe --list --running --quiet" => Ok("firecrab-debian\n".into()),
            _ => Ok(String::new()),
        });
        stop().expect("stops");
        let calls = wsl.calls();
        let position = |needle: &str| {
            calls
                .iter()
                .position(|call| call.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing from {calls:?}"))
        };
        assert!(position("/DISABLE") < position("/End"));
        assert!(
            position("systemctl stop firecrab-api firecrab-net-helper") < position("--terminate")
        );
    }

    #[test]
    fn stop_on_an_uninstalled_host_does_nothing() {
        let wsl = fake::answer(|line| match line {
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            _ => Err("not found".into()),
        });
        stop().expect("nothing to stop");
        assert_eq!(wsl.calls().len(), 2, "one task query, one running check");
    }

    #[test]
    fn uninstall_deletes_the_task_and_its_definition() {
        let (_directory, layout) = layout();
        let definition = layout.runtime().join("microManager-task.xml");
        fs::write(&definition, b"x").expect("definition");
        let wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(DISABLED.into()),
            "wsl.exe --list --running --quiet" => Ok(String::new()),
            _ => Ok(String::new()),
        });
        uninstall(&layout).expect("uninstalled");
        assert!(!definition.exists());
        assert!(
            wsl.calls()
                .contains(&"schtasks.exe /Delete /TN Firecrab\\microManager /F".to_string())
        );
        uninstall(&layout).expect("a missing definition is fine");
    }

    #[test]
    fn status_asks_a_running_guest_for_its_units() {
        let _wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(ENABLED.into()),
            "wsl.exe --list --running --quiet" => Ok("firecrab-debian\n".into()),
            l if l.contains("systemctl is-active") => {
                Ok("firecrab-api=active\nfirecrab-net-helper=active\nip=172.20.1.5\n".into())
            }
            other => panic!("unexpected {other}"),
        });
        let status = status();
        assert_eq!(status.task, TaskState::Enabled);
        assert!(status.ready);
        assert!(
            status
                .detail
                .as_deref()
                .is_some_and(|detail| detail.ends_with("ip=172.20.1.5"))
        );
    }

    #[test]
    fn status_never_boots_a_stopped_guest() {
        let wsl = fake::answer(|line| match line {
            l if l.starts_with("schtasks.exe /Query") => Ok(DISABLED.into()),
            "wsl.exe --list --running --quiet" => Ok("Debian\n".into()),
            other => panic!("a stopped guest must not be queried: {other}"),
        });
        let status = status();
        assert!(!status.ready);
        assert_eq!(status.detail.as_deref(), Some("not running"));
        assert_eq!(wsl.calls().len(), 2);
    }

    #[test]
    fn a_guest_query_failure_is_reported_as_the_detail() {
        let _wsl = fake::answer(|line| match line {
            "wsl.exe --list --running --quiet" => Ok("firecrab-debian\n".into()),
            l if l.contains("systemctl") => Err("Catastrophic failure".into()),
            _ => Ok(ENABLED.into()),
        });
        let status = status();
        assert!(!status.ready);
        assert!(
            status
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Catastrophic failure"))
        );
    }

    #[test]
    fn task_definitions_are_written_as_utf16_with_a_bom() {
        let bytes = utf16_with_bom("<a/>");
        assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
        assert_eq!(wsl::decode_console(&bytes), "<a/>");
    }

    #[test]
    fn only_the_settings_block_decides_whether_the_task_is_enabled() {
        let enabled = "<Triggers><TimeTrigger><Enabled>false</Enabled></TimeTrigger></Triggers>\
                       <Settings><Enabled>true</Enabled></Settings>";
        assert!(settings_enabled(enabled));
        let disabled = "<Triggers><LogonTrigger><Enabled>true</Enabled></LogonTrigger></Triggers>\
                        <Settings><Hidden>true</Hidden><Enabled>false</Enabled></Settings>";
        assert!(!settings_enabled(disabled));
        assert!(settings_enabled(
            "<Settings><Hidden>true</Hidden></Settings>"
        ));
    }

    #[test]
    fn the_guest_is_ready_only_when_every_unit_is_active() {
        let (ready, detail) =
            parse_units("firecrab-api=active\nfirecrab-net-helper=active\nip=172.20.1.5\n");
        assert!(ready);
        assert_eq!(
            detail.as_deref(),
            Some("firecrab-api=active, firecrab-net-helper=active, ip=172.20.1.5")
        );
        let (ready, _) = parse_units("firecrab-api=activating\nfirecrab-net-helper=active\n");
        assert!(!ready);
    }

    #[test]
    fn success_needs_the_task_the_guest_and_the_api() {
        let healthy = Status {
            task: TaskState::Enabled,
            ready: true,
            api_reachable: true,
            detail: None,
        };
        assert!(healthy.success());
        for broken in [
            Status {
                task: TaskState::Disabled,
                ..healthy.clone()
            },
            Status {
                ready: false,
                ..healthy.clone()
            },
            Status {
                api_reachable: false,
                ..healthy.clone()
            },
        ] {
            assert!(!broken.success());
        }
    }
}
