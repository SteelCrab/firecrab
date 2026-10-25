use std::fs::{self, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::thread;
use std::time::{Duration, Instant};

use firecrab_api_types::HostOs;

use super::super::dev::instance;
use super::super::host_platform::{self, HOST_PLATFORM_DESCRIPTOR_PATH};
use super::super::{manifest, report, runtime::Runtime};
use super::lifecycle::Layout;

const LABEL: &str = "io.firecrab.micromanager";
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct DaemonPaths {
    pub label: String,
    pub api_port: u16,
    pub plist: PathBuf,
    pub wrapper: PathBuf,
    pub ready: PathBuf,
    pub manager_ready: PathBuf,
    pub log: PathBuf,
    /// This Mac's description, pushed into the guest for `GET /api/host`.
    pub platform: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub loaded: bool,
    pub ready: bool,
    pub api_reachable: bool,
    pub detail: Option<String>,
}

impl Status {
    pub fn success(&self) -> bool {
        self.loaded && self.ready && self.api_reachable
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Manifest(#[from] manifest::Error),
    #[error("HOME is not set; launchd service installation needs a user home")]
    MissingHome,
    #[error("the loaded microManager service belongs to another managed home; refusing to stop it")]
    ForeignService,
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("launchctl {action} failed: {detail}")]
    Launchctl {
        action: &'static str,
        detail: String,
    },
    #[error("management daemon did not become ready within {0} seconds")]
    ReadyTimeout(u64),
    #[error(transparent)]
    Processes(#[from] super::processes::Error),
    #[error("management VM did not stop within {0} seconds")]
    StopTimeout(u64),
    #[error("invalid manager-ready marker: {0}")]
    InvalidMarker(String),
    #[error("microManager service registration is missing ({0})")]
    NotInstalled(PathBuf),
}

pub fn install(
    layout: &Layout,
    helper: &Path,
    ssh_private_key: &Path,
) -> Result<DaemonPaths, Error> {
    install_with_runtime(
        layout,
        helper,
        ssh_private_key,
        &Runtime::load(&layout.managed_home)?,
    )
}

pub fn install_with_runtime(
    layout: &Layout,
    helper: &Path,
    ssh_private_key: &Path,
    runtime: &Runtime,
) -> Result<DaemonPaths, Error> {
    let paths = paths(layout)?;
    install_at(layout, helper, ssh_private_key, &paths, runtime)?;
    Ok(paths)
}

fn install_at(
    layout: &Layout,
    helper: &Path,
    ssh_private_key: &Path,
    paths: &DaemonPaths,
    settings: &Runtime,
) -> Result<(), Error> {
    let parent = paths.plist.parent().ok_or_else(|| Error::Io {
        action: "resolve LaunchAgents directory",
        path: paths.plist.clone(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "plist has no parent"),
    })?;
    fs::create_dir_all(parent)
        .map_err(|source| io_error("create LaunchAgents directory", parent, source))?;
    let runtime = paths.wrapper.parent().expect("wrapper has runtime parent");
    fs::create_dir_all(runtime)
        .map_err(|source| io_error("create daemon runtime directory", runtime, source))?;

    super::ssh::prepare(runtime, &settings.ssh)?;
    let mut wrapper = render_wrapper(layout, helper, ssh_private_key, paths, settings);
    let mut plist = render_plist(paths, settings);
    if layout.managed_home.join("runtime/controlled").is_file() {
        let settings = super::super::settings::Store::new(&layout.managed_home)
            .load()
            .map_err(|error| {
                io_error(
                    "read settings",
                    &layout.managed_home,
                    io::Error::other(error),
                )
            })?
            .settings;
        wrapper = wrapper.replace("127.0.0.1:5523:127.0.0.1:5523", "127.0.0.1:5524:127.0.0.1:5523")
            .replace("http://127.0.0.1:5523/api/host", "http://127.0.0.1:5524/api/host")
            .replace("FIRECRAB_MICROMANAGER_HOME=\"$root\"", &format!("FIRECRAB_MICROMANAGER_CPU={} FIRECRAB_MICROMANAGER_MEMORY_MIB={} FIRECRAB_MICROMANAGER_HOME=\"$root\"", settings.cpu, settings.memory_mib));
        plist = plist
            .replace(
                "<key>RunAtLoad</key><true/>",
                "<key>RunAtLoad</key><false/>",
            )
            .replace(
                "<key>KeepAlive</key><true/>",
                "<key>KeepAlive</key><false/>",
            );
    }
    write_atomic(&paths.wrapper, wrapper.as_bytes(), 0o700)?;
    write_atomic(&paths.plist, plist.as_bytes(), 0o600)?;
    Ok(())
}

pub fn uninstall(layout: &Layout) -> Result<(), Error> {
    stop(layout)?;
    let paths = paths(layout)?;
    for path in [paths.plist, paths.wrapper, paths.ready] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("remove daemon file", &path, source)),
        }
    }
    Ok(())
}

pub fn start(layout: &Layout) -> Result<Status, Error> {
    start_with_runtime(layout, &Runtime::load(&layout.managed_home)?)
}

pub fn start_with_runtime(layout: &Layout, settings: &Runtime) -> Result<Status, Error> {
    settings.validate()?;
    let paths = paths(layout)?;
    require_installed(&paths)?;
    stop_if_loaded(layout)?;
    install_at(
        layout,
        &layout.helper_path(),
        &layout.managed_home.join("runtime/manager_ed25519"),
        &paths,
        settings,
    )?;
    write_host_platform(&paths)?;
    let _ = fs::remove_file(&paths.ready);
    let _ = fs::remove_file(&paths.manager_ready);
    report!("[START] load the launchd service and boot the management VM");
    launchctl(
        "bootstrap",
        &[
            domain(layout)?.as_str(),
            paths.plist.to_string_lossy().as_ref(),
        ],
        false,
    )?;
    if layout.managed_home.join("runtime/controlled").is_file() {
        launchctl(
            "kickstart",
            &[&format!("{}/{}", domain(layout)?, paths.label)],
            false,
        )?;
    }
    let timeout = READY_TIMEOUT;
    report!(
        "[WAIT] VM/API readiness (up to {}s); console: {}",
        timeout.as_secs(),
        layout.managed_home.join("runtime/vm-console.log").display()
    );
    wait_ready(
        timeout,
        || status(layout),
        |elapsed, status| {
            report!(
                "[WAIT] VM/API: {}s/{}s; launchd loaded={}, VM ready={}, API reachable={}",
                elapsed.as_secs(),
                timeout.as_secs(),
                status.loaded,
                status.ready,
                status.api_reachable
            );
        },
    )
}

/// `launchctl bootstrap` of a missing plist reports only "Input/output error".
fn require_installed(paths: &DaemonPaths) -> Result<(), Error> {
    match [&paths.plist, &paths.wrapper]
        .into_iter()
        .find(|path| !path.is_file())
    {
        Some(missing) => Err(Error::NotInstalled(missing.clone())),
        None => Ok(()),
    }
}

pub fn stop(layout: &Layout) -> Result<(), Error> {
    stop_if_loaded(layout)
}

pub fn status(layout: &Layout) -> Result<Status, Error> {
    let paths = paths(layout)?;
    let service = format!("{}/{}", domain(layout)?, paths.label);
    let loaded = ProcessCommand::new("/bin/launchctl")
        .args(["print", &service])
        .output()
        .map_err(|source| io_error("query launchd service", &paths.plist, source))?
        .status
        .success();
    let detail = if paths.ready.is_file() {
        Some(
            fs::read_to_string(&paths.ready)
                .map_err(|source| io_error("read daemon marker", &paths.ready, source))?,
        )
    } else {
        None
    };
    let ready = detail.is_some();
    let api_reachable = api_reachable(layout, &paths);
    Ok(Status {
        loaded,
        ready,
        api_reachable,
        detail,
    })
}

pub fn paths(layout: &Layout) -> Result<DaemonPaths, Error> {
    let state = instance::load(&layout.managed_home)
        .map_err(|source| io_error("read development instance", &layout.managed_home, source))?;
    if state.is_some() && layout.managed_home.join("runtime/controlled").exists() {
        return Err(io_error(
            "validate development instance",
            &layout.managed_home,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "development instances cannot use the shared VM controller",
            ),
        ));
    }
    let label = instance::label(&layout.managed_home, LABEL).map_err(|source| {
        io_error(
            "read development service label",
            &layout.managed_home,
            source,
        )
    })?;
    let api_port = instance::api_port(&layout.managed_home)
        .map_err(|source| io_error("read development API port", &layout.managed_home, source))?;
    let runtime = layout.managed_home.join("runtime");
    // Private VMs are started explicitly and must not all boot at the next login.
    let plist = if state.is_some() {
        runtime.join(format!("{label}.plist"))
    } else {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(Error::MissingHome)?;
        home.join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    };
    Ok(DaemonPaths {
        label,
        api_port,
        plist,
        wrapper: runtime.join("daemon.sh"),
        ready: runtime.join("daemon-ready"),
        manager_ready: runtime.join("manager-ready"),
        log: runtime.join("daemon.log"),
        platform: runtime.join("host-platform.json"),
    })
}

/// Refreshed on every start, since a macOS update changes the version. The
/// wrapper pushes it into the guest once the tunnel is up.
fn write_host_platform(paths: &DaemonPaths) -> Result<(), Error> {
    let version = ProcessCommand::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let controller_id = paths
        .platform
        .parent()
        .and_then(Path::parent)
        .and_then(|home| super::super::resource::controller_id(home).ok());
    let descriptor = host_platform::descriptor_with_controller(
        HostOs::Macos,
        "macOS",
        &version,
        "Virtualization.framework",
        controller_id,
    );
    write_atomic(&paths.platform, format!("{descriptor}\n").as_bytes(), 0o600)
}

fn wait_ready(
    timeout: Duration,
    mut probe: impl FnMut() -> Result<Status, Error>,
    mut progress: impl FnMut(Duration, &Status),
) -> Result<Status, Error> {
    let started = Instant::now();
    let mut next_report = Duration::ZERO;
    while started.elapsed() < timeout {
        let status = probe()?;
        if status.success() || !status.loaded {
            return Ok(status);
        }
        let elapsed = started.elapsed();
        if elapsed >= next_report {
            progress(elapsed, &status);
            next_report = elapsed + PROGRESS_INTERVAL;
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(Error::ReadyTimeout(timeout.as_secs()))
}

fn stop_if_loaded(layout: &Layout) -> Result<(), Error> {
    let paths = paths(layout)?;
    let vm_pid = fs::read_to_string(layout.managed_home.join("runtime/vm-process"))
        .or_else(|_| fs::read_to_string(&paths.ready))
        .ok()
        .and_then(|marker| vm_pid_from_marker(&marker));
    let domain = domain(layout)?;
    let service = format!("{domain}/{}", paths.label);
    let registered = ProcessCommand::new("/bin/launchctl")
        .args(["print", &service])
        .output()
        .map_err(|source| io_error("query launchd service", &paths.plist, source))?;
    if registered.status.success() {
        if !registered_wrapper_matches(&registered.stdout, &paths.wrapper) {
            return Err(Error::ForeignService);
        }
        // The registered job can outlive a deleted or damaged plist.
        report!("[STOP] request launchd service and management VM shutdown");
        launchctl("bootout", &[&service], false)?;
    }
    super::processes::stop_helpers(layout)?;
    let started = Instant::now();
    let mut next_report = Duration::ZERO;
    loop {
        let still_loaded = ProcessCommand::new("/bin/launchctl")
            .args(["print", &service])
            .output()
            .map_err(|source| io_error("query stopped launchd service", &paths.plist, source))?
            .status
            .success();
        let vm_alive = vm_pid.is_some_and(|pid| {
            ProcessCommand::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
        if !still_loaded && !vm_alive {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(40) {
            return Err(Error::StopTimeout(40));
        }
        if elapsed >= next_report {
            report!(
                "[WAIT] service shutdown: {}s/40s; launchd loaded={}, VM running={}",
                elapsed.as_secs(),
                still_loaded,
                vm_alive
            );
            next_report = elapsed + PROGRESS_INTERVAL;
        }
        thread::sleep(Duration::from_secs(1));
    }
    let _ = fs::remove_file(paths.ready);
    let _ = fs::remove_file(layout.managed_home.join("runtime/vm-process"));
    Ok(())
}

fn registered_wrapper_matches(output: &[u8], wrapper: &Path) -> bool {
    let text = String::from_utf8_lossy(output);
    let Some(arguments) = text.split_once("arguments = {\n").map(|(_, rest)| rest) else {
        return false;
    };
    arguments
        .lines()
        .map(str::trim)
        .take_while(|line| *line != "}")
        .any(|line| Path::new(line) == wrapper)
}

fn vm_pid_from_marker(marker: &str) -> Option<u32> {
    marker
        .lines()
        .find_map(|line| line.strip_prefix("vm_pid="))?
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
}

#[test]
fn stopped_service_marker_requires_a_valid_vm_pid() {
    assert_eq!(
        vm_pid_from_marker("vm_pid=123\ntunnel_pid=456\nip=192.0.2.7\n"),
        Some(123)
    );
    for marker in [
        "",
        "vm_pid=0",
        "vm_pid=1",
        "vm_pid=-1",
        "vm_pid=abc",
        "tunnel_pid=123",
    ] {
        assert_eq!(vm_pid_from_marker(marker), None);
    }
}

fn launchctl(
    action: &'static str,
    arguments: &[&str],
    tolerate_failure: bool,
) -> Result<(), Error> {
    let output = ProcessCommand::new("/bin/launchctl")
        .arg(action)
        .args(arguments)
        .output()
        .map_err(|source| Error::Io {
            action: "run launchctl",
            path: PathBuf::from("/bin/launchctl"),
            source,
        })?;
    if output.status.success() || tolerate_failure {
        Ok(())
    } else {
        Err(Error::Launchctl {
            action,
            detail: output_detail(&output),
        })
    }
}

fn domain(_layout: &Layout) -> Result<String, Error> {
    let output = ProcessCommand::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|source| Error::Io {
            action: "resolve user id",
            path: PathBuf::from("/usr/bin/id"),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::Launchctl {
            action: "resolve user id",
            detail: output_detail(&output),
        });
    }
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

fn api_reachable(layout: &Layout, paths: &DaemonPaths) -> bool {
    let port = if layout.managed_home.join("runtime/controlled").is_file() {
        5524
    } else {
        paths.api_port
    };
    let url = format!("http://127.0.0.1:{port}/api/host");
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(url).send())
        .is_ok_and(|response| response.status().is_success())
}

fn private_tunnel_start(api_port: u16, control_attempts: u32) -> String {
    format!(
        r#"  rm -f "$tunnel_control" || return 1
  /usr/bin/ssh -S "$tunnel_control" -o ControlMaster=yes -o ControlPersist=no \
    -i "$key" -o BatchMode=yes "${{tunnel_ssh_options[@]}}" \
    -o StrictHostKeyChecking=accept-new -o "$known_hosts_option" \
    -o ExitOnForwardFailure=yes -N "root@$ip" &
  tunnel_pid=$!
  local master_ready=false
  for _ in $(seq 1 {control_attempts}); do
    kill -0 "$tunnel_pid" 2>/dev/null || return 1
    if /usr/bin/ssh -S "$tunnel_control" -O check "root@$ip" >/dev/null 2>&1; then
      master_ready=true
      break
    fi
    sleep 0.1
  done
  if [ "$master_ready" != true ] || ! /usr/bin/ssh -S "$tunnel_control" \
    -o ExitOnForwardFailure=yes -O forward \
    -L 127.0.0.1:{api_port}:127.0.0.1:5523 "root@$ip"; then
    kill -TERM "$tunnel_pid" 2>/dev/null || true
    wait "$tunnel_pid" 2>/dev/null || true
    return 1
  fi"#
    )
}

fn render_wrapper(
    layout: &Layout,
    helper: &Path,
    ssh_private_key: &Path,
    paths: &DaemonPaths,
    settings: &Runtime,
) -> String {
    let ssh_options = |session| {
        super::ssh::arguments(&layout.managed_home.join("runtime"), &settings.ssh, session)
            .iter()
            .map(|arg| shell_quote(Path::new(arg)))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let command_ssh_options = ssh_options(super::ssh::Session::Command);
    let tunnel_ssh_options = ssh_options(super::ssh::Session::Tunnel);
    let reconnect_attempts = settings.ssh.reconnect_attempts;
    let reconnect_delay = settings.ssh.reconnect_delay_secs;
    let restart_vm = settings.ssh.restart_vm_on_failure;
    let api_port = paths.api_port;
    let (tunnel_setup, tunnel_connect, tunnel_confirm, tunnel_cleanup) = if paths.label != LABEL {
        (
            "tunnel_control_dir=$(/usr/bin/mktemp -d /private/tmp/fc-dev-api.XXXXXXXX)\nchmod 0700 \"$tunnel_control_dir\"\ntunnel_control=\"$tunnel_control_dir/api\"",
            private_tunnel_start(
                api_port,
                settings
                    .ssh
                    .connect_timeout_secs
                    .saturating_add(5)
                    .saturating_mul(10),
            ),
            "if ! /usr/bin/ssh -S \"$tunnel_control\" -O check \"root@$ip\" >/dev/null 2>&1; then\n        kill -TERM \"$tunnel_pid\" 2>/dev/null || true\n        wait \"$tunnel_pid\" 2>/dev/null || true\n        return 1\n      fi",
            "if [ -n \"$tunnel_control\" ]; then rm -f \"$tunnel_control\"; fi\n  if [ -n \"$tunnel_control_dir\" ]; then rmdir \"$tunnel_control_dir\" 2>/dev/null || true; fi",
        )
    } else {
        (
            "",
            format!(
                r#"  /usr/bin/ssh -i "$key" -o BatchMode=yes "${{tunnel_ssh_options[@]}}" \
    -o StrictHostKeyChecking=accept-new -o "$known_hosts_option" \
    -o ExitOnForwardFailure=yes \
    -N -L 127.0.0.1:{api_port}:127.0.0.1:5523 "root@$ip" &
  tunnel_pid=$!"#
            ),
            ":",
            "",
        )
    };
    let root = shell_quote(&layout.managed_home);
    let install_dir = shell_quote(&layout.install_dir);
    let cli = shell_quote(&layout.cli_path());
    let helper = shell_quote(helper);
    let key = shell_quote(ssh_private_key);
    let ready = shell_quote(&paths.ready);
    let manager_ready = shell_quote(&paths.manager_ready);
    let console = shell_quote(&layout.managed_home.join("runtime/vm-console.log"));
    let known_hosts = shell_quote(&layout.managed_home.join("runtime/known_hosts"));
    let known_hosts_option = shell_quote(Path::new(&super::known_hosts_option(
        &layout.managed_home.join("runtime/known_hosts"),
    )));
    let platform = shell_quote(&paths.platform);
    let platform_write = shell_quote(Path::new(&format!(
        "mkdir -p /etc/firecrab && cat > {HOST_PLATFORM_DESCRIPTOR_PATH}"
    )));
    format!(
        r#"#!/bin/bash
set -Eeuo pipefail
root={root}
export FIRECRAB_INSTALL_DIR={install_dir}
cli={cli}
helper={helper}
key={key}
ready={ready}
manager_ready={manager_ready}
console={console}
known_hosts={known_hosts}
known_hosts_option={known_hosts_option}
platform={platform}
command_ssh_options=({command_ssh_options})
tunnel_ssh_options=({tunnel_ssh_options})
vm_process="$root/runtime/vm-process"
vm_pid=
tunnel_pid=
relay_pid=
resource_pid=
tunnel_control_dir=
tunnel_control=
cleanup() {{
  trap - EXIT INT TERM
  set +e
  rm -f "$ready"
  if [ -n "$resource_pid" ] && kill -0 "$resource_pid" 2>/dev/null; then
    kill -TERM "$resource_pid" 2>/dev/null
    wait "$resource_pid" 2>/dev/null
  fi
  if [ -n "$relay_pid" ] && kill -0 "$relay_pid" 2>/dev/null; then
    kill -TERM "$relay_pid" 2>/dev/null
    wait "$relay_pid" 2>/dev/null
  fi
  if [ -n "$tunnel_pid" ] && kill -0 "$tunnel_pid" 2>/dev/null; then
    kill -TERM "$tunnel_pid" 2>/dev/null
    wait "$tunnel_pid" 2>/dev/null
  fi
  {tunnel_cleanup}
  if [ -n "$vm_pid" ] && kill -0 "$vm_pid" 2>/dev/null; then
    ip=$(sed -n 's/^ip=//p' "$manager_ready" 2>/dev/null)
    if [ -n "$ip" ]; then
      /usr/bin/ssh -i "$key" -o ConnectTimeout=3 -o BatchMode=yes "${{command_ssh_options[@]}}" \
        -o StrictHostKeyChecking=accept-new -o "$known_hosts_option" \
        "root@$ip" systemctl poweroff >/dev/null 2>&1 || true
    fi
    kill -TERM "$vm_pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
      kill -0 "$vm_pid" 2>/dev/null || break
      sleep 1
    done
    kill -KILL "$vm_pid" 2>/dev/null || true
    wait "$vm_pid" 2>/dev/null || true
  fi
  if [ "$(sed -n 's/^vm_pid=//p' "$vm_process" 2>/dev/null)" = "$vm_pid" ]; then
    rm -f "$vm_process"
  fi
}}
trap cleanup EXIT INT TERM
{tunnel_setup}
rm -f "$ready" "$manager_ready"
FIRECRAB_MICROMANAGER_HOME="$root" FIRECRAB_MICROMANAGER_HELPER="$helper" \
  "$cli" service resource-server --home "$root" &
resource_pid=$!
FIRECRAB_MICROMANAGER_HOME="$root" "$helper" run >>"$console" 2>&1 &
vm_pid=$!
printf 'vm_pid=%s\n' "$vm_pid" >"$vm_process.tmp"
mv -f "$vm_process.tmp" "$vm_process"
for _ in $(seq 1 120); do
  [ -f "$manager_ready" ] && break
  kill -0 "$vm_pid" 2>/dev/null || exit 1
  sleep 1
done
test -f "$manager_ready"
ip=$(sed -n 's/^ip=//p' "$manager_ready")
test -n "$ip"
open_tunnel() {{
{tunnel_connect}
  for _ in $(seq 1 60); do
    if /usr/bin/curl -fsS --max-time 2 http://127.0.0.1:{api_port}/api/host >/dev/null; then
      {tunnel_confirm}
      return 0
    fi
    kill -0 "$tunnel_pid" 2>/dev/null || return 1
    sleep 1
  done
  kill -TERM "$tunnel_pid" 2>/dev/null
  wait "$tunnel_pid" 2>/dev/null
  return 1
}}
publish_ready() {{
  {{
    echo "vm_pid=$vm_pid"
    echo "tunnel_pid=$tunnel_pid"
    echo "ip=$ip"
  }} >"$ready"
}}
reconnects=0
until open_tunnel; do
  wait "$tunnel_pid" 2>/dev/null || true
  kill -0 "$vm_pid" 2>/dev/null || exit 1
  reconnects=$((reconnects + 1))
  if [ "$reconnects" -gt {reconnect_attempts} ] && {restart_vm}; then exit 1; fi
  echo "Initial API tunnel unavailable; retrying ($reconnects)"
  sleep {reconnect_delay}
done
# Tells the guest's API which Mac it serves; failing to only costs the Host view.
if [ -f "$platform" ]; then
  /usr/bin/ssh -i "$key" -o BatchMode=yes "${{command_ssh_options[@]}}" \
    -o StrictHostKeyChecking=accept-new -o "$known_hosts_option" \
    "root@$ip" {platform_write} <"$platform" || true
fi
# Serves running VMs' TCP port forwards on 127.0.0.1. A crash restarts only the
# relay; the VM and its workloads keep running.
"$cli" service forward-ports --key "$key" --known-hosts "$known_hosts" "$ip" &
relay_pid=$!
publish_ready
set +e
# A dropped tunnel (sleep, a network stall) only reconnects: restarting the VM
# would take every microVM inside it down too. A guest that stays unreachable
# through every attempt is restarted like a crashed one.
reconnects=0
while :; do
  if ! kill -0 "$vm_pid" 2>/dev/null; then
    wait "$vm_pid"
    rc=$?
    rm -f "$ready"
    exit "$rc"
  fi
  if ! kill -0 "$tunnel_pid" 2>/dev/null; then
    wait "$tunnel_pid" 2>/dev/null
    rm -f "$ready"
    reconnects=$((reconnects + 1))
    if [ "$reconnects" -gt {reconnect_attempts} ]; then
      if {restart_vm}; then exit 1; fi
      echo "API tunnel reconnect limit reached; VM restart disabled"
      reconnects=0
    fi
    echo "API tunnel exited; reconnecting ($reconnects/{reconnect_attempts})"
    if open_tunnel; then
      reconnects=0
      publish_ready
    else
      sleep {reconnect_delay}
    fi
  fi
  if ! kill -0 "$relay_pid" 2>/dev/null; then
    wait "$relay_pid" 2>/dev/null
    sleep 5
    "$cli" service forward-ports --key "$key" --known-hosts "$known_hosts" "$ip" &
    relay_pid=$!
  fi
  if ! kill -0 "$resource_pid" 2>/dev/null; then
    wait "$resource_pid" 2>/dev/null
    sleep 5
    FIRECRAB_MICROMANAGER_HOME="$root" FIRECRAB_MICROMANAGER_HELPER="$helper" \
      "$cli" service resource-server --home "$root" &
    resource_pid=$!
  fi
  sleep 1
done
"#
    )
}

fn render_plist(paths: &DaemonPaths, settings: &Runtime) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>{wrapper}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><{auto_restart}/>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ExitTimeOut</key><integer>45</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        auto_restart = settings.auto_restart,
        label = xml_escape(&paths.label),
        wrapper = xml_escape(&paths.wrapper.to_string_lossy()),
        log = xml_escape(&paths.log.to_string_lossy()),
    )
}

fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<(), Error> {
    let parent = path.parent().ok_or_else(|| Error::Io {
        action: "resolve file parent",
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| io_error("create staged daemon file", path, source))?;
    staged
        .write_all(contents)
        .map_err(|source| io_error("write daemon file", staged.path(), source))?;
    staged
        .flush()
        .map_err(|source| io_error("flush daemon file", staged.path(), source))?;
    staged
        .as_file()
        .set_permissions(Permissions::from_mode(mode))
        .map_err(|source| io_error("set daemon file permissions", staged.path(), source))?;
    staged
        .persist(path)
        .map_err(|error| io_error("publish daemon file", path, error.error))?;
    Ok(())
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn output_detail(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        output.status.to_string()
    } else {
        detail.to_string()
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn loaded_service_must_use_the_requested_home_wrapper() {
        let wrapper = Path::new("/tmp/home with spaces/runtime/daemon.sh");
        let output = b"job = {\n\targuments = {\n\t\t/bin/bash\n\t\t/tmp/home with spaces/runtime/daemon.sh\n\t}\n}\n";
        assert!(registered_wrapper_matches(output, wrapper));
        assert!(!registered_wrapper_matches(
            output,
            Path::new("/tmp/other/runtime/daemon.sh")
        ));
        assert!(!registered_wrapper_matches(
            b"stdout path = /tmp/home with spaces/runtime/daemon.sh\n",
            wrapper
        ));
    }

    fn pending_status() -> Status {
        Status {
            loaded: true,
            ready: false,
            api_reachable: false,
            detail: None,
        }
    }

    #[test]
    fn readiness_progress_precedes_success_and_keeps_the_health_check() {
        let mut probes = 0;
        let mut updates = Vec::new();
        let result = wait_ready(
            Duration::from_secs(5),
            || {
                probes += 1;
                let mut status = pending_status();
                if probes == 2 {
                    status.ready = true;
                    status.api_reachable = true;
                }
                Ok(status)
            },
            |elapsed, status| updates.push((elapsed, status.success())),
        )
        .unwrap();
        assert!(result.success());
        assert_eq!(probes, 2);
        assert_eq!(updates.len(), 1);
        assert!(!updates[0].1);
    }

    #[test]
    fn an_unhealthy_vm_reports_again_while_waiting_and_still_times_out() {
        let mut updates = Vec::new();
        let result = wait_ready(
            Duration::from_secs(12),
            || Ok(pending_status()),
            |elapsed, status| updates.push((elapsed, status.clone())),
        );
        assert!(matches!(result, Err(Error::ReadyTimeout(12))));
        assert!(
            updates.len() >= 2,
            "waiting must remain visible: {updates:?}"
        );
        for pair in updates.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= PROGRESS_INTERVAL);
        }
        assert!(updates.iter().all(|(_, status)| !status.success()));
    }

    #[test]
    fn a_lost_service_or_probe_error_is_not_hidden_by_progress() {
        let mut stopped = pending_status();
        stopped.loaded = false;
        let result = wait_ready(
            Duration::from_secs(5),
            || Ok(stopped.clone()),
            |_, _| panic!("a stopped service must return immediately"),
        )
        .unwrap();
        assert!(!result.success());
        let result = wait_ready(
            Duration::from_secs(5),
            || Err(Error::InvalidMarker("bad marker".to_string())),
            |_, _| panic!("a failed probe must return immediately"),
        );
        assert!(matches!(result, Err(Error::InvalidMarker(_))));
    }

    #[test]
    fn repairing_registration_preserves_installed_binaries_disks_and_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let layout = Layout {
            install_dir: root.join("bin"),
            managed_home: root.join("micromanager"),
        };
        let paths = DaemonPaths {
            label: LABEL.into(),
            api_port: 5523,
            plist: root.join("LaunchAgents/agent.plist"),
            wrapper: layout.managed_home.join("runtime/daemon.sh"),
            ready: layout.managed_home.join("runtime/daemon-ready"),
            manager_ready: layout.managed_home.join("runtime/manager-ready"),
            log: layout.managed_home.join("runtime/daemon.log"),
            platform: layout.managed_home.join("runtime/host-platform.json"),
        };
        let key = layout.managed_home.join("runtime/manager_ed25519");
        let preserved = [
            layout.cli_path(),
            layout.helper_path(),
            layout.managed_home.join("system/debian-system.raw"),
            layout.managed_home.join("data/firecrab-data.raw"),
            key.clone(),
            layout.managed_home.join("runtime/known_hosts"),
            layout.managed_home.join("runtime/provisioned"),
        ];
        for path in &preserved {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"preserved").unwrap();
        }
        // Recover both a damaged wrapper and a completely missing plist.
        fs::write(&paths.wrapper, b"broken").unwrap();
        install_at(
            &layout,
            &layout.helper_path(),
            &key,
            &paths,
            &Runtime::default(),
        )
        .unwrap();
        assert!(fs::read_to_string(&paths.plist).unwrap().contains(LABEL));
        assert!(
            fs::read_to_string(&paths.wrapper)
                .unwrap()
                .starts_with("#!/bin/bash\n")
        );
        assert_eq!(
            fs::metadata(&paths.wrapper).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&paths.plist).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for path in preserved {
            assert_eq!(fs::read(path).unwrap(), b"preserved");
        }
        assert!(!layout.managed_home.join("downloads").exists());
    }

    #[test]
    fn controlled_registration_preserves_ssh_policy_and_resource_service() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout {
            install_dir: directory.path().join("bin"),
            managed_home: directory.path().join("micromanager"),
        };
        let runtime = layout.managed_home.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::write(runtime.join("controlled"), b"1\n").unwrap();
        let store = super::super::super::settings::Store::new(&layout.managed_home);
        let snapshot = store.load().unwrap();
        let mut settings = snapshot.settings.clone();
        settings.cpu = 4;
        settings.memory_mib = 8192;
        store.save(&snapshot, settings).unwrap();
        let paths = DaemonPaths {
            label: LABEL.into(),
            api_port: 5523,
            plist: directory.path().join("LaunchAgents/agent.plist"),
            wrapper: runtime.join("daemon.sh"),
            ready: runtime.join("daemon-ready"),
            manager_ready: runtime.join("manager-ready"),
            log: runtime.join("daemon.log"),
            platform: runtime.join("host-platform.json"),
        };
        let mut policy = Runtime::default();
        policy.ssh.keepalive_interval_secs = 23;
        install_at(
            &layout,
            &layout.helper_path(),
            &runtime.join("key"),
            &paths,
            &policy,
        )
        .unwrap();
        let wrapper = fs::read_to_string(&paths.wrapper).unwrap();
        assert!(wrapper.contains("127.0.0.1:5524:127.0.0.1:5523"));
        assert!(wrapper.contains("http://127.0.0.1:5524/api/host"));
        assert!(wrapper.contains("FIRECRAB_MICROMANAGER_CPU=4"));
        assert!(wrapper.contains("FIRECRAB_MICROMANAGER_MEMORY_MIB=8192"));
        assert!(wrapper.contains("ServerAliveInterval=23"));
        assert!(wrapper.contains("service resource-server --home"));
        let plist = fs::read_to_string(&paths.plist).unwrap();
        assert!(plist.contains("<key>RunAtLoad</key><false/>"));
        assert!(plist.contains("<key>KeepAlive</key><false/>"));
        assert!(
            ProcessCommand::new("/bin/bash")
                .arg("-n")
                .arg(&paths.wrapper)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn wrapper_supervises_vm_tunnel_and_escalating_shutdown() {
        let layout = Layout {
            install_dir: PathBuf::from("/tmp/firecrab bin"),
            managed_home: PathBuf::from("/tmp/firecrab state/micromanager"),
        };
        let paths = DaemonPaths {
            label: LABEL.into(),
            api_port: 5523,
            plist: PathBuf::from("/tmp/agent.plist"),
            wrapper: layout.managed_home.join("runtime/daemon.sh"),
            ready: layout.managed_home.join("runtime/daemon-ready"),
            manager_ready: layout.managed_home.join("runtime/manager-ready"),
            log: layout.managed_home.join("runtime/daemon.log"),
            platform: layout.managed_home.join("runtime/host-platform.json"),
        };
        let script = render_wrapper(
            &layout,
            &layout.install_dir.join("firecrab-micromanager-macos"),
            &layout.managed_home.join("runtime/manager_ed25519"),
            &paths,
            &Runtime::default(),
        );
        assert!(script.contains("ExitOnForwardFailure=yes"));
        assert!(script.contains("127.0.0.1:5523:127.0.0.1:5523"));
        assert!(script.contains("cli='/tmp/firecrab bin/firecrab'"));
        assert!(script.contains("export FIRECRAB_INSTALL_DIR='/tmp/firecrab bin'"));
        assert!(script.contains(
            "\"$cli\" service forward-ports --key \"$key\" --known-hosts \"$known_hosts\" \"$ip\" &"
        ));
        assert!(script.contains("kill -TERM \"$relay_pid\""));
        assert!(script.contains("systemctl poweroff"));
        assert!(script.contains("kill -KILL"));
        assert!(script.contains("'/tmp/firecrab state/micromanager'"));
        assert!(script.contains(
            "'mkdir -p /etc/firecrab && cat > /etc/firecrab/host-platform.json' <\"$platform\" || true"
        ));
    }

    #[test]
    fn private_service_label_and_api_port_reach_the_plist_and_tunnel() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout {
            install_dir: directory.path().join("private-bin"),
            managed_home: directory.path().join("private/micromanager"),
        };
        let paths = DaemonPaths {
            label: "io.firecrab.micromanager.dev-a1b2c3d4".into(),
            api_port: 60230,
            plist: directory.path().join("private.plist"),
            wrapper: layout.managed_home.join("runtime/daemon.sh"),
            ready: layout.managed_home.join("runtime/daemon-ready"),
            manager_ready: layout.managed_home.join("runtime/manager-ready"),
            log: layout.managed_home.join("runtime/daemon.log"),
            platform: layout.managed_home.join("runtime/host-platform.json"),
        };
        let script = render_wrapper(
            &layout,
            &layout.helper_path(),
            &layout.managed_home.join("runtime/manager_ed25519"),
            &paths,
            &Runtime::default(),
        );
        assert!(script.contains("-L 127.0.0.1:60230:127.0.0.1:5523"));
        assert!(script.contains("http://127.0.0.1:60230/api/host"));
        assert!(!script.contains("-L 127.0.0.1:5523:"));
        assert!(!script.contains("http://127.0.0.1:5523/api/host"));
        assert!(script.contains(&format!(
            "export FIRECRAB_INSTALL_DIR={}",
            shell_quote(&layout.install_dir)
        )));
        let plist = render_plist(&paths, &Runtime::default());
        assert!(
            plist
                .contains("<key>Label</key><string>io.firecrab.micromanager.dev-a1b2c3d4</string>")
        );
        assert!(!plist.contains("<key>Label</key><string>io.firecrab.micromanager</string>"));
        assert!(plist.contains(paths.wrapper.to_str().unwrap()));
    }

    #[test]
    fn default_paths_keep_the_primary_service_label_and_api_port() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout {
            install_dir: directory.path().join("bin"),
            managed_home: directory.path().join("micromanager"),
        };
        let paths = paths(&layout).unwrap();
        assert_eq!(paths.label, LABEL);
        assert_eq!(paths.api_port, 5523);
        assert_eq!(
            paths.plist.file_name().unwrap(),
            "io.firecrab.micromanager.plist"
        );
        assert!(
            paths
                .plist
                .ends_with("Library/LaunchAgents/io.firecrab.micromanager.plist")
        );
        assert_eq!(paths.wrapper, layout.managed_home.join("runtime/daemon.sh"));
    }

    #[test]
    fn private_paths_use_only_the_selected_instances_service_and_port() {
        let directory = tempfile::tempdir().unwrap();
        let state = instance::test_instance(directory.path(), "0123456789abcdef", 60230, 60231);
        let layout = Layout {
            install_dir: state.install_dir,
            managed_home: state.managed_home,
        };
        let paths = paths(&layout).unwrap();
        assert_eq!(paths.label, "io.firecrab.micromanager.dev-0123456789abcdef");
        assert_eq!(paths.api_port, 60230);
        assert_eq!(
            paths.plist.file_name().unwrap(),
            "io.firecrab.micromanager.dev-0123456789abcdef.plist"
        );
        assert_eq!(
            paths.plist.parent().unwrap(),
            layout.managed_home.join("runtime")
        );
        assert_eq!(
            paths.ready,
            layout.managed_home.join("runtime/daemon-ready")
        );
    }

    #[test]
    fn private_instances_cannot_select_the_primary_sleepy_controller() {
        let directory = tempfile::tempdir().unwrap();
        let state = instance::test_instance(directory.path(), "0123456789abcdef", 60230, 60231);
        let layout = Layout {
            install_dir: state.install_dir,
            managed_home: state.managed_home,
        };
        fs::create_dir_all(layout.managed_home.join("runtime")).unwrap();
        fs::write(layout.managed_home.join("runtime/controlled"), b"1\n").unwrap();
        assert!(matches!(
            paths(&layout),
            Err(Error::Io {
                action: "validate development instance",
                ..
            })
        ));
    }

    #[test]
    fn broken_instance_state_cannot_select_the_primary_service() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout {
            install_dir: directory.path().join("bin"),
            managed_home: directory.path().join("micromanager"),
        };
        fs::create_dir_all(&layout.managed_home).unwrap();
        fs::write(
            layout.managed_home.join("dev-instance.json"),
            "invalid instance",
        )
        .unwrap();
        assert!(matches!(
            paths(&layout),
            Err(Error::Io {
                action: "read development instance",
                ..
            })
        ));
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
    }

    fn read_marker(path: &Path) -> Option<HashMap<String, String>> {
        let text = fs::read_to_string(path).ok()?;
        let marker: HashMap<_, _> = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        (marker.len() == 3).then_some(marker)
    }

    fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(30) {
            if let Some(value) = probe() {
                return value;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}");
    }

    fn signal(pid: &str, signal: &str) {
        ProcessCommand::new("/bin/kill")
            .args([signal, pid])
            .status()
            .unwrap();
    }

    /// Runs the real wrapper with stand-ins for the helper, ssh, and curl, then
    /// kills the tunnel the way a sleep or network stall would.
    #[test]
    fn a_dropped_tunnel_reconnects_without_restarting_the_vm() {
        reconnect_after_failure(Runtime::default(), 0);
    }

    #[test]
    fn exhausted_ssh_retries_can_leave_the_vm_running_and_reconnect_later() {
        let mut settings = Runtime::default();
        settings.ssh.restart_vm_on_failure = false;
        settings.ssh.reconnect_attempts = 0;
        settings.ssh.reconnect_delay_secs = 1;
        reconnect_after_failure(settings, 2);
    }

    #[test]
    fn an_initial_ssh_failure_can_retry_without_stopping_the_vm() {
        let mut settings = Runtime::default();
        settings.ssh.restart_vm_on_failure = false;
        settings.ssh.reconnect_attempts = 0;
        settings.ssh.reconnect_delay_secs = 1;
        reconnect_after_failure(settings, 1);
    }

    fn reconnect_after_failure(settings: Runtime, failure_at: usize) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let layout = Layout {
            install_dir: root.join("bin"),
            managed_home: root.join("Application Support/micromanager"),
        };
        let runtime = layout.managed_home.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let paths = DaemonPaths {
            label: LABEL.into(),
            api_port: 5523,
            plist: root.join("agent.plist"),
            wrapper: runtime.join("daemon.sh"),
            ready: runtime.join("daemon-ready"),
            manager_ready: runtime.join("manager-ready"),
            log: runtime.join("daemon.log"),
            platform: runtime.join("host-platform.json"),
        };
        let helper = root.join("helper");
        write_executable(
            &helper,
            "#!/bin/bash\necho ip=192.0.2.7 >\"$FIRECRAB_MICROMANAGER_HOME/runtime/manager-ready\"\nexec sleep 600\n",
        );
        let tunnels = root.join("tunnels");
        let live_tunnel = root.join("live-tunnel");
        let ssh = root.join("ssh");
        write_executable(
            &ssh,
            &format!(
                "#!/bin/bash\nactual=$(/usr/bin/ssh -G \"$@\" 2>/dev/null | sed -n 's/^userknownhostsfile //p')\ntest \"$actual\" = {} || exit 1\ncase \" $* \" in *' -N '*) echo tunnel >>{}; count=$(wc -l <{}); if [ \"$count\" -eq {} ]; then exit 1; fi; echo $$ >{}; exec sleep 600;; esac\ncat >/dev/null\n",
                shell_quote(&runtime.join("known_hosts")),
                shell_quote(&tunnels),
                shell_quote(&tunnels),
                failure_at,
                shell_quote(&live_tunnel),
            ),
        );
        let curl = root.join("curl");
        write_executable(
            &curl,
            &format!(
                "#!/bin/bash\npid=$(cat {} 2>/dev/null)\ntest -n \"$pid\" && kill -0 \"$pid\" 2>/dev/null\n",
                shell_quote(&live_tunnel)
            ),
        );
        // The installed CLI serves port forwards beside the tunnel.
        fs::create_dir_all(&layout.install_dir).unwrap();
        write_executable(&layout.cli_path(), "#!/bin/bash\nexec sleep 600\n");
        let script = render_wrapper(&layout, &helper, &runtime.join("key"), &paths, &settings)
            .replace("/usr/bin/ssh", &ssh.to_string_lossy())
            .replace("/usr/bin/curl", &curl.to_string_lossy());
        write_executable(&paths.wrapper, &script);

        let mut wrapper = ProcessCommand::new("/bin/bash")
            .arg(&paths.wrapper)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let first = wait_for("the first ready marker", || read_marker(&paths.ready));
        wait_for("the first tunnel invocation", || {
            (fs::read_to_string(&tunnels).ok()?.lines().count()
                == if failure_at == 1 { 2 } else { 1 })
            .then_some(())
        });
        signal(&first["tunnel_pid"], "-KILL");
        let second = wait_for("a reconnected tunnel", || {
            read_marker(&paths.ready).filter(|marker| {
                marker["tunnel_pid"] != first["tunnel_pid"]
                    && fs::read_to_string(&tunnels).is_ok_and(|text| {
                        text.lines().count() == if failure_at > 0 { 3 } else { 2 }
                    })
            })
        });

        assert_eq!(second["vm_pid"], first["vm_pid"], "the VM keeps running");
        wait_for("the second tunnel invocation", || {
            (fs::read_to_string(&tunnels).ok()?.lines().count()
                == if failure_at > 0 { 3 } else { 2 })
            .then_some(())
        });
        assert!(
            wrapper.try_wait().unwrap().is_none(),
            "the wrapper keeps supervising"
        );

        signal(&wrapper.id().to_string(), "-TERM");
        wait_for("the wrapper to stop", || wrapper.try_wait().unwrap());
        let vm_alive = ProcessCommand::new("/bin/kill")
            .args(["-0", &first["vm_pid"]])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!vm_alive, "stopping the wrapper stops the VM");
    }

    #[test]
    fn a_private_tunnel_rejects_a_foreign_api_before_http_readiness() {
        private_tunnel_acknowledgment(false, false);
    }

    #[test]
    fn a_private_tunnel_publishes_ready_only_after_forward_acknowledgment() {
        private_tunnel_acknowledgment(true, true);
    }

    fn private_tunnel_acknowledgment(forward_succeeds: bool, reuse_connections: bool) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let layout = Layout {
            install_dir: root.join("bin"),
            managed_home: root.join("micromanager"),
        };
        let runtime = layout.managed_home.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&layout.install_dir).unwrap();
        let occupied_api =
            (!forward_succeeds).then(|| std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap());
        let api_port = occupied_api
            .as_ref()
            .map_or(60230, |listener| listener.local_addr().unwrap().port());
        let paths = DaemonPaths {
            label: format!("{LABEL}.dev-0123456789abcdef"),
            api_port,
            plist: runtime.join("agent.plist"),
            wrapper: runtime.join("daemon.sh"),
            ready: runtime.join("daemon-ready"),
            manager_ready: runtime.join("manager-ready"),
            log: runtime.join("daemon.log"),
            platform: runtime.join("host-platform.json"),
        };
        let events = root.join("events");
        let master = root.join("master");
        let control_path = root.join("control-path");
        let master_arguments = root.join("master-arguments");
        let helper = root.join("helper");
        write_executable(
            &helper,
            "#!/bin/bash\necho ip=192.0.2.7 >\"$FIRECRAB_MICROMANAGER_HOME/runtime/manager-ready\"\nexec sleep 600\n",
        );
        write_executable(&layout.cli_path(), "#!/bin/bash\nexec sleep 600\n");
        let ssh = root.join("ssh");
        write_executable(
            &ssh,
            &format!(
                r#"#!/bin/bash
set -eu
case " $* " in
  *' -O check '*)
    test -f "$2" || exit 1
    pid=$(cat {master})
    kill -0 "$pid" 2>/dev/null
    ;;
  *' -O forward '*)
    echo forward >>{events}
    test ! -f {ready} || exit 98
    {forward_succeeds} || exit 1
    echo acknowledged >>{events}
    ;;
  *' -N '*)
    test "$1" = -S && test "$3" = -o && test "$4" = ControlMaster=yes
    test "$5" = -o && test "$6" = ControlPersist=no
    printf '%s\n' "$@" >{master_arguments}
    printf '%s\n' "$2" >{control_path}
    echo $$ >{master}
    touch "$2"
    exec sleep 600
    ;;
  *) exit 0 ;;
esac
"#,
                master = shell_quote(&master),
                events = shell_quote(&events),
                ready = shell_quote(&paths.ready),
                forward_succeeds = forward_succeeds,
                master_arguments = shell_quote(&master_arguments),
                control_path = shell_quote(&control_path),
            ),
        );
        // Even an unrelated process returning HTTP 200 must not bypass the ACK.
        let curl = root.join("curl");
        write_executable(
            &curl,
            &format!(
                "#!/bin/bash\necho curl >>{}\nexit 0\n",
                shell_quote(&events)
            ),
        );
        let mut settings = Runtime::default();
        settings.ssh.reconnect_attempts = 0;
        settings.ssh.reuse_connections = reuse_connections;
        let script = render_wrapper(&layout, &helper, &runtime.join("key"), &paths, &settings)
            .replace("/usr/bin/ssh", &ssh.to_string_lossy())
            .replace("/usr/bin/curl", &curl.to_string_lossy());
        write_executable(&paths.wrapper, &script);
        let mut wrapper = ProcessCommand::new("/bin/bash")
            .arg(&paths.wrapper)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let control = wait_for("the private control socket path", || {
            fs::read_to_string(&control_path)
                .ok()
                .map(|path| PathBuf::from(path.trim()))
        });
        let control_directory = control.parent().unwrap();
        if forward_succeeds {
            wait_for("the acknowledged private API", || read_marker(&paths.ready));
            assert_eq!(
                fs::metadata(control_directory)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let events = fs::read_to_string(&events).unwrap();
            assert_eq!(
                events.lines().collect::<Vec<_>>(),
                ["forward", "acknowledged", "curl"]
            );
            assert!(wrapper.try_wait().unwrap().is_none());
            signal(&wrapper.id().to_string(), "-TERM");
        }
        let status = wait_for("the private wrapper to stop", || {
            wrapper.try_wait().unwrap()
        });
        if !forward_succeeds {
            assert!(!status.success());
            assert_eq!(fs::read_to_string(&events).unwrap(), "forward\n");
            assert!(!paths.ready.exists());
        }
        let arguments = fs::read_to_string(&master_arguments).unwrap();
        assert!(!arguments.lines().any(|argument| argument == "-L"));
        assert!(
            !control_directory.exists(),
            "only this wrapper's control directory is removed"
        );
    }

    #[test]
    fn start_names_what_is_missing_instead_of_a_launchctl_error() {
        let directory = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            label: LABEL.into(),
            api_port: 5523,
            plist: directory.path().join("agent.plist"),
            wrapper: directory.path().join("daemon.sh"),
            ready: directory.path().join("ready"),
            manager_ready: directory.path().join("manager-ready"),
            log: directory.path().join("daemon.log"),
            platform: directory.path().join("host-platform.json"),
        };
        let Err(Error::NotInstalled(missing)) = require_installed(&paths) else {
            panic!("a missing plist must be reported");
        };
        assert_eq!(missing, paths.plist);

        fs::write(&paths.plist, b"plist").unwrap();
        fs::write(&paths.wrapper, b"wrapper").unwrap();
        assert!(require_installed(&paths).is_ok());
    }

    #[test]
    fn plist_is_a_keepalive_background_launch_agent() {
        let paths = DaemonPaths {
            label: LABEL.into(),
            api_port: 5523,
            plist: PathBuf::from("/tmp/agent.plist"),
            wrapper: PathBuf::from("/tmp/daemon.sh"),
            ready: PathBuf::from("/tmp/ready"),
            manager_ready: PathBuf::from("/tmp/manager-ready"),
            log: PathBuf::from("/tmp/daemon.log"),
            platform: PathBuf::from("/tmp/host-platform.json"),
        };
        let plist = render_plist(&paths, &Runtime::default());
        assert!(plist.contains(LABEL));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
        assert!(plist.contains("<key>KeepAlive</key><true/>"));
        assert!(plist.contains("<key>ExitTimeOut</key><integer>45</integer>"));
        assert!(plist.contains("<key>ProcessType</key><string>Background</string>"));
        let manual = Runtime {
            auto_restart: false,
            ..Runtime::default()
        };
        let plist = render_plist(&paths, &manual);
        assert!(plist.contains("<key>KeepAlive</key><false/>"));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    }

    #[test]
    fn quoting_and_xml_escaping_cover_spaces_quotes_and_ampersands() {
        assert_eq!(shell_quote(Path::new("/tmp/a'b")), "'/tmp/a'\\''b'");
        assert_eq!(xml_escape("a&<>'\""), "a&amp;&lt;&gt;&apos;&quot;");
    }
}
