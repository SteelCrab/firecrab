use std::fs::{self, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};
use std::thread;
use std::time::{Duration, Instant};

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
    stop_if_loaded(layout)?;
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
    })
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
    let helper = shell_quote(helper);
    let key = shell_quote(ssh_private_key);
    let ready = shell_quote(&paths.ready);
    let manager_ready = shell_quote(&paths.manager_ready);
    let console = shell_quote(&layout.managed_home.join("runtime/vm-console.log"));
    let known_hosts = shell_quote(&layout.managed_home.join("runtime/known_hosts"));
    format!(
        r#"#!/bin/bash
set -Eeuo pipefail
root={root}
helper={helper}
key={key}
ready={ready}
manager_ready={manager_ready}
console={console}
known_hosts={known_hosts}
vm_pid=
tunnel_pid=
cleanup() {{
  trap - EXIT INT TERM
  set +e
  rm -f "$ready"
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
/usr/bin/ssh -i "$key" -o BatchMode=yes -o ConnectTimeout=10 \
  -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$known_hosts" \
  -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
  -N -L 127.0.0.1:5523:127.0.0.1:5523 "root@$ip" &
tunnel_pid=$!
for _ in $(seq 1 60); do
  if /usr/bin/curl -fsS --max-time 2 http://127.0.0.1:5523/api/host >/dev/null; then break; fi
  kill -0 "$tunnel_pid" 2>/dev/null || exit 1
  sleep 1
done
/usr/bin/curl -fsS --max-time 2 http://127.0.0.1:5523/api/host >/dev/null
{{
  echo "vm_pid=$vm_pid"
  echo "tunnel_pid=$tunnel_pid"
  echo "ip=$ip"
}} >"$ready"
set +e
wait "$vm_pid"
rc=$?
exit "$rc"
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
        };
        let script = render_wrapper(
            &layout,
            &layout.install_dir.join("firecrab-micromanager-macos"),
            &layout.managed_home.join("runtime/manager_ed25519"),
            &paths,
        );
        assert!(script.contains("ExitOnForwardFailure=yes"));
        assert!(script.contains("127.0.0.1:5523:127.0.0.1:5523"));
        assert!(script.contains("systemctl poweroff"));
        assert!(script.contains("kill -KILL"));
        assert!(script.contains("'/tmp/firecrab state/micromanager'"));
    }

    #[test]
    fn plist_is_a_keepalive_background_launch_agent() {
        let paths = DaemonPaths {
            plist: PathBuf::from("/tmp/agent.plist"),
            wrapper: PathBuf::from("/tmp/daemon.sh"),
            ready: PathBuf::from("/tmp/ready"),
            manager_ready: PathBuf::from("/tmp/manager-ready"),
            log: PathBuf::from("/tmp/daemon.log"),
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
