use std::fs::{self, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::thread;
use std::time::{Duration, Instant};

use firecrab_api_types::HostOs;

use super::super::host_platform::{self, HOST_PLATFORM_DESCRIPTOR_PATH};
use super::lifecycle::Layout;

const LABEL: &str = "io.firecrab.micromanager";
const API_URL: &str = "http://127.0.0.1:5523/api/host";

#[derive(Clone, Debug)]
pub struct DaemonPaths {
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
    #[error("HOME is not set; launchd service installation needs a user home")]
    MissingHome,
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
    #[error("invalid manager-ready marker: {0}")]
    InvalidMarker(String),
    #[error("microManager is not installed ({0} is missing); run `firecrab service install`")]
    NotInstalled(PathBuf),
}

pub fn install(
    layout: &Layout,
    helper: &Path,
    ssh_private_key: &Path,
) -> Result<DaemonPaths, Error> {
    let paths = paths(layout)?;
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

    write_atomic(
        &paths.wrapper,
        render_wrapper(layout, helper, ssh_private_key, &paths).as_bytes(),
        0o700,
    )?;
    write_atomic(&paths.plist, render_plist(&paths).as_bytes(), 0o600)?;
    Ok(paths)
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
    let paths = paths(layout)?;
    require_installed(&paths)?;
    stop_if_loaded(layout)?;
    write_host_platform(&paths)?;
    let _ = fs::remove_file(&paths.ready);
    let _ = fs::remove_file(&paths.manager_ready);
    launchctl(
        "bootstrap",
        &[
            domain(layout)?.as_str(),
            paths.plist.to_string_lossy().as_ref(),
        ],
        false,
    )?;
    wait_ready(layout, Duration::from_secs(180))
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
    let service = format!("{}/{}", domain(layout)?, LABEL);
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
    let api_reachable = api_reachable();
    Ok(Status {
        loaded,
        ready,
        api_reachable,
        detail,
    })
}

pub fn paths(layout: &Layout) -> Result<DaemonPaths, Error> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(Error::MissingHome)?;
    let runtime = layout.managed_home.join("runtime");
    Ok(DaemonPaths {
        plist: home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
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
    let descriptor = host_platform::descriptor_json(
        HostOs::Macos,
        "macOS",
        &version,
        "Virtualization.framework",
    );
    write_atomic(&paths.platform, format!("{descriptor}\n").as_bytes(), 0o600)
}

fn wait_ready(layout: &Layout, timeout: Duration) -> Result<Status, Error> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let status = status(layout)?;
        if status.success() {
            return Ok(status);
        }
        if !status.loaded {
            return Ok(status);
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(Error::ReadyTimeout(timeout.as_secs()))
}

fn stop_if_loaded(layout: &Layout) -> Result<(), Error> {
    let paths = paths(layout)?;
    let domain = domain(layout)?;
    let service = format!("{domain}/{LABEL}");
    let loaded = ProcessCommand::new("/bin/launchctl")
        .args(["print", &service])
        .output()
        .map_err(|source| io_error("query launchd service", &paths.plist, source))?
        .status
        .success();
    if loaded {
        launchctl(
            "bootout",
            &[domain.as_str(), paths.plist.to_string_lossy().as_ref()],
            false,
        )?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(40) {
            let still_loaded = ProcessCommand::new("/bin/launchctl")
                .args(["print", &service])
                .output()
                .map_err(|source| io_error("query stopped launchd service", &paths.plist, source))?
                .status
                .success();
            if !still_loaded {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
    let _ = fs::remove_file(paths.ready);
    Ok(())
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

fn api_reachable() -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .and_then(|client| client.get(API_URL).send())
        .is_ok_and(|response| response.status().is_success())
}

fn render_wrapper(
    layout: &Layout,
    helper: &Path,
    ssh_private_key: &Path,
    paths: &DaemonPaths,
) -> String {
    let root = shell_quote(&layout.managed_home);
    let cli = shell_quote(&layout.cli_path());
    let helper = shell_quote(helper);
    let key = shell_quote(ssh_private_key);
    let ready = shell_quote(&paths.ready);
    let manager_ready = shell_quote(&paths.manager_ready);
    let console = shell_quote(&layout.managed_home.join("runtime/vm-console.log"));
    let known_hosts = shell_quote(&layout.managed_home.join("runtime/known_hosts"));
    let platform = shell_quote(&paths.platform);
    let platform_write = shell_quote(Path::new(&format!(
        "mkdir -p /etc/firecrab && cat > {HOST_PLATFORM_DESCRIPTOR_PATH}"
    )));
    format!(
        r#"#!/bin/bash
set -Eeuo pipefail
root={root}
cli={cli}
helper={helper}
key={key}
ready={ready}
manager_ready={manager_ready}
console={console}
known_hosts={known_hosts}
platform={platform}
vm_pid=
tunnel_pid=
relay_pid=
cleanup() {{
  trap - EXIT INT TERM
  set +e
  rm -f "$ready"
  if [ -n "$relay_pid" ] && kill -0 "$relay_pid" 2>/dev/null; then
    kill -TERM "$relay_pid" 2>/dev/null
    wait "$relay_pid" 2>/dev/null
  fi
  if [ -n "$tunnel_pid" ] && kill -0 "$tunnel_pid" 2>/dev/null; then
    kill -TERM "$tunnel_pid" 2>/dev/null
    wait "$tunnel_pid" 2>/dev/null
  fi
  if [ -n "$vm_pid" ] && kill -0 "$vm_pid" 2>/dev/null; then
    ip=$(sed -n 's/^ip=//p' "$manager_ready" 2>/dev/null)
    if [ -n "$ip" ]; then
      /usr/bin/ssh -i "$key" -o BatchMode=yes -o ConnectTimeout=3 \
        -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$known_hosts" \
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
}}
trap cleanup EXIT INT TERM
rm -f "$ready" "$manager_ready"
FIRECRAB_MICROMANAGER_HOME="$root" "$helper" run >>"$console" 2>&1 &
vm_pid=$!
for _ in $(seq 1 120); do
  [ -f "$manager_ready" ] && break
  kill -0 "$vm_pid" 2>/dev/null || exit 1
  sleep 1
done
test -f "$manager_ready"
ip=$(sed -n 's/^ip=//p' "$manager_ready")
test -n "$ip"
open_tunnel() {{
  /usr/bin/ssh -i "$key" -o BatchMode=yes -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$known_hosts" \
    -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
    -N -L 127.0.0.1:5523:127.0.0.1:5523 "root@$ip" &
  tunnel_pid=$!
  for _ in $(seq 1 60); do
    if /usr/bin/curl -fsS --max-time 2 http://127.0.0.1:5523/api/host >/dev/null; then return 0; fi
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
open_tunnel
# Tells the guest's API which Mac it serves; failing to only costs the Host view.
if [ -f "$platform" ]; then
  /usr/bin/ssh -i "$key" -o BatchMode=yes -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$known_hosts" \
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
    [ "$reconnects" -le 5 ] || exit 1
    echo "API tunnel exited; reconnecting ($reconnects/5)"
    if open_tunnel; then
      reconnects=0
      publish_ready
    else
      sleep 5
    fi
  fi
  if ! kill -0 "$relay_pid" 2>/dev/null; then
    wait "$relay_pid" 2>/dev/null
    sleep 5
    "$cli" service forward-ports --key "$key" --known-hosts "$known_hosts" "$ip" &
    relay_pid=$!
  fi
  sleep 1
done
"#
    )
}

fn render_plist(paths: &DaemonPaths) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>{wrapper}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label = LABEL,
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
    fn wrapper_supervises_vm_tunnel_and_escalating_shutdown() {
        let layout = Layout {
            install_dir: PathBuf::from("/tmp/firecrab bin"),
            managed_home: PathBuf::from("/tmp/firecrab state/micromanager"),
        };
        let paths = DaemonPaths {
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
        );
        assert!(script.contains("ExitOnForwardFailure=yes"));
        assert!(script.contains("127.0.0.1:5523:127.0.0.1:5523"));
        assert!(script.contains("cli='/tmp/firecrab bin/firecrab'"));
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
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let layout = Layout {
            install_dir: root.join("bin"),
            managed_home: root.join("micromanager"),
        };
        let runtime = layout.managed_home.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let paths = DaemonPaths {
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
        let ssh = root.join("ssh");
        write_executable(
            &ssh,
            &format!(
                "#!/bin/bash\ncase \" $* \" in *' -N '*) echo tunnel >>{}; exec sleep 600;; esac\ncat >/dev/null\n",
                shell_quote(&tunnels)
            ),
        );
        let curl = root.join("curl");
        write_executable(&curl, "#!/bin/bash\nexit 0\n");
        // The installed CLI serves port forwards beside the tunnel.
        fs::create_dir_all(&layout.install_dir).unwrap();
        write_executable(&layout.cli_path(), "#!/bin/bash\nexec sleep 600\n");
        let script = render_wrapper(&layout, &helper, &runtime.join("key"), &paths)
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
        signal(&first["tunnel_pid"], "-KILL");
        let second = wait_for("a reconnected tunnel", || {
            read_marker(&paths.ready).filter(|marker| marker["tunnel_pid"] != first["tunnel_pid"])
        });

        assert_eq!(second["vm_pid"], first["vm_pid"], "the VM keeps running");
        assert_eq!(fs::read_to_string(&tunnels).unwrap().lines().count(), 2);
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
    fn start_names_what_is_missing_instead_of_a_launchctl_error() {
        let directory = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
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
        assert!(
            Error::NotInstalled(missing)
                .to_string()
                .contains("run `firecrab service install`")
        );

        fs::write(&paths.plist, b"plist").unwrap();
        fs::write(&paths.wrapper, b"wrapper").unwrap();
        assert!(require_installed(&paths).is_ok());
    }

    #[test]
    fn plist_is_a_keepalive_background_launch_agent() {
        let paths = DaemonPaths {
            plist: PathBuf::from("/tmp/agent.plist"),
            wrapper: PathBuf::from("/tmp/daemon.sh"),
            ready: PathBuf::from("/tmp/ready"),
            manager_ready: PathBuf::from("/tmp/manager-ready"),
            log: PathBuf::from("/tmp/daemon.log"),
            platform: PathBuf::from("/tmp/host-platform.json"),
        };
        let plist = render_plist(&paths);
        assert!(plist.contains(LABEL));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
        assert!(plist.contains("<key>KeepAlive</key><true/>"));
        assert!(plist.contains("<key>ProcessType</key><string>Background</string>"));
    }

    #[test]
    fn quoting_and_xml_escaping_cover_spaces_quotes_and_ampersands() {
        assert_eq!(shell_quote(Path::new("/tmp/a'b")), "'/tmp/a'\\''b'");
        assert_eq!(xml_escape("a&<>'\""), "a&amp;&lt;&gt;&apos;&quot;");
    }
}
