//! Firecrab **Metrics Agent** — guest-side CPU/memory reporter.
//!
//! Installed into each VM rootfs at specialize time (not baked into templates).
//! Reports `FIRECRAB_USAGE …` lines on the serial console for
//! [`crate::process_metrics`] to parse. Kept out of [`crate::rootfs`] so disk
//! prepare/grow stays independent of observability payload.

use std::path::Path;

use crate::rootfs::{
    RootfsError, guest_path_exists, remove_from_image, run_debugfs, set_guest_file_mode,
    write_into_image,
};

/// Guest binary path (POSIX shell script).
///
/// Under `/usr/local/sbin` on Ubuntu/Rocky and Alpine (Alpine only ships
/// `/usr/local/{bin,lib,share}` — we create `sbin` when needed).
pub(crate) const BIN_PATH: &str = "/usr/local/sbin/firecrab-guest-agent";
pub(crate) const UNIT_PATH: &str = "/etc/systemd/system/firecrab-guest-agent.service";
pub(crate) const WANTS_PATH: &str =
    "/etc/systemd/system/multi-user.target.wants/firecrab-guest-agent.service";
pub(crate) const OPENRC_PATH: &str = "/etc/init.d/firecrab-guest-agent";
pub(crate) const OPENRC_RUNLEVEL_PATH: &str = "/etc/runlevels/default/firecrab-guest-agent";

/// systemd unit name (`systemctl start …`).
pub(crate) const SERVICE_NAME: &str = "firecrab-guest-agent.service";

/// Reports guest OS CPU % and memory used (MemTotal − MemAvailable) on the
/// serial console every few seconds for the host to parse (`FIRECRAB_USAGE`).
pub(crate) const AGENT_SCRIPT: &str = r#"#!/bin/sh
# Firecrab Metrics Agent (console transport v1).
# Guest OS metrics only — not host Firecracker process RSS.
# Avoid `set -e`: a single awk/test failure must not kill the reporter loop.
INTERVAL="${FIRECRAB_GUEST_AGENT_INTERVAL:-3}"
STATE=/run/firecrab-guest-agent.cpu
mkdir -p /run
echo $$ >/run/firecrab-guest-agent.pid 2>/dev/null || true

read_cpu() {
  awk '/^cpu / {
    idle=$5; total=0
    for (i=2; i<=NF; i++) total+=$i
    print total, idle
    exit
  }' /proc/stat 2>/dev/null
}

read_mem() {
  awk '
    /^MemTotal:/ { t=$2+0 }
    /^MemAvailable:/ { a=$2+0 }
    END {
      if (t <= 0) { print "0 0 0.0"; exit }
      u = t - a
      if (u < 0) u = 0
      printf "%d %d %.1f\n", int(u/1024), int(t/1024), (u*100)/t
    }
  ' /proc/meminfo 2>/dev/null
}

read_cpu >"$STATE" 2>/dev/null || echo "0 0" >"$STATE"
sleep 1

while true; do
  set -- $(read_cpu)
  total=${1:-0}
  idle=${2:-0}
  set -- $(cat "$STATE" 2>/dev/null)
  prev_total=${1:-0}
  prev_idle=${2:-0}
  printf '%s %s\n' "$total" "$idle" >"$STATE" 2>/dev/null || true

  cpu_pct="0.0"
  if [ "$total" -gt "$prev_total" ] 2>/dev/null; then
    dt=$((total - prev_total))
    di=$((idle - prev_idle))
    if [ "$dt" -gt 0 ] 2>/dev/null; then
      busy=$((dt - di))
      if [ "$busy" -lt 0 ] 2>/dev/null; then busy=0; fi
      cpu_pct=$(awk -v b="$busy" -v d="$dt" 'BEGIN { printf "%.1f", (b*100)/d }' 2>/dev/null) || cpu_pct="0.0"
    fi
  fi

  set -- $(read_mem)
  mem_used_mib=${1:-0}
  mem_total_mib=${2:-0}
  mem_used_pct=${3:-0.0}

  # Host console reader parses this; WebSocket terminal strips the line.
  printf 'FIRECRAB_USAGE cpu_pct=%s mem_used_mib=%s mem_total_mib=%s mem_used_pct=%s\n' \
    "$cpu_pct" "$mem_used_mib" "$mem_total_mib" "$mem_used_pct" >/dev/console 2>/dev/null || true

  sleep "$INTERVAL"
done
"#;

const SYSTEMD_UNIT: &str = r#"[Unit]
Description=Firecrab Metrics Agent
After=local-fs.target

[Service]
Type=simple
# /bin/sh so the unit works even if the script is not chmod +x yet.
ExecStart=/bin/sh /usr/local/sbin/firecrab-guest-agent
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"#;

/// OpenRC long-running service (Alpine). `command_background` keeps the
/// reporter alive after the service's start() returns — unlike a bare
/// background job under a oneshot network-ready service.
const OPENRC_SERVICE: &str = r#"#!/sbin/openrc-run
description="Firecrab Metrics Agent"

command="/bin/sh"
command_args="/usr/local/sbin/firecrab-guest-agent"
command_background=true
pidfile="/run/firecrab-guest-agent.openrc.pid"

depend() {
	after localmount
}
"#;

/// Installs the metrics agent on Ubuntu/Rocky (systemd) and Alpine (OpenRC).
/// No-ops only when the image has neither `/usr/local` nor an existing
/// `/usr/local/sbin` (nothing we can write into offline).
pub(crate) fn install(rootfs: &Path) -> Result<(), RootfsError> {
    if !ensure_bin_dir(rootfs)? {
        return Ok(());
    }
    write_into_image(rootfs, BIN_PATH, AGENT_SCRIPT.as_bytes())?;
    set_guest_file_mode(rootfs, BIN_PATH, "0100755");

    if guest_path_exists(rootfs, "/etc/systemd/system") {
        write_into_image(rootfs, UNIT_PATH, SYSTEMD_UNIT.as_bytes())?;
        // systemd only honors *symlinks* under multi-user.target.wants
        // (a regular file is ignored with "is not a symlink").
        // debugfs syntax: `symlink <linkpath> <target>`.
        if guest_path_exists(rootfs, "/etc/systemd/system/multi-user.target.wants") {
            ensure_symlink(rootfs, WANTS_PATH, UNIT_PATH)?;
        }
    }

    // Alpine (and any OpenRC image): enable a long-running service so the
    // agent is not tied to firecrab-network-ready's oneshot lifetime.
    if guest_path_exists(rootfs, "/etc/init.d") {
        write_into_image(rootfs, OPENRC_PATH, OPENRC_SERVICE.as_bytes())?;
        set_guest_file_mode(rootfs, OPENRC_PATH, "0100755");
        if guest_path_exists(rootfs, "/etc/runlevels/default") {
            ensure_symlink(rootfs, OPENRC_RUNLEVEL_PATH, OPENRC_PATH)?;
        }
    }
    Ok(())
}

/// Ensures `/usr/local/sbin` exists so the agent script can be installed.
/// Alpine templates ship `/usr/local` without `sbin`.
fn ensure_bin_dir(rootfs: &Path) -> Result<bool, RootfsError> {
    if guest_path_exists(rootfs, "/usr/local/sbin") {
        return Ok(true);
    }
    if !guest_path_exists(rootfs, "/usr/local") {
        return Ok(false);
    }
    let _ = run_debugfs(rootfs, "mkdir /usr/local/sbin");
    if !guest_path_exists(rootfs, "/usr/local/sbin") {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: "debugfs failed to create /usr/local/sbin for metrics agent".into(),
        });
    }
    Ok(true)
}

/// Creates `link` → `target` inside the image and verifies the link exists.
/// debugfs may exit 0 even when the command failed, so we check positively.
fn ensure_symlink(rootfs: &Path, link: &str, target: &str) -> Result<(), RootfsError> {
    remove_from_image(rootfs, link);
    let _ = run_debugfs(rootfs, &format!("symlink {link} {target}"));
    if !guest_path_exists(rootfs, link) {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("debugfs failed to create symlink {link} → {target}"),
        });
    }
    Ok(())
}

/// Shell fragment injected into the systemd network-ready oneshot so the
/// metrics agent starts as its **own** unit (survives oneshot exit /
/// `KillMode=control-group`).
pub(crate) fn network_ready_kick_systemd() -> String {
    format!(
        r#"# Start the long-lived Metrics Agent unit (separate from this oneshot).
# Background `exec sh` under Type=oneshot is killed with KillMode=control-group
# when this script exits — so always prefer systemctl for the metrics unit.
# Metrics go to /dev/console; host terminal UI strips FIRECRAB_USAGE lines.
if command -v systemctl >/dev/null 2>&1 \
  && [ -f {UNIT_PATH} ]; then
  systemctl start {SERVICE_NAME} >/dev/null 2>&1 || true
else
  (
    agent={BIN_PATH}
    [ -f "$agent" ] || exit 0
    chmod 755 "$agent" 2>/dev/null || true
    if [ -f /run/firecrab-guest-agent.pid ]; then
      old=$(cat /run/firecrab-guest-agent.pid 2>/dev/null || true)
      if [ -n "$old" ] && kill -0 "$old" 2>/dev/null; then
        exit 0
      fi
    fi
    exec sh "$agent"
  ) >/dev/null 2>&1 &
fi
"#
    )
}

/// Shell fragment for Alpine OpenRC network-ready `start()` — early kick;
/// runlevel enable is the primary start path.
pub(crate) fn network_ready_kick_openrc() -> String {
    format!(
        r#"	# Prefer the dedicated OpenRC service (long-lived). Fall back to a
	# start-stop-daemon launch if the service file is missing.
	if [ -x {OPENRC_PATH} ]; then
		{OPENRC_PATH} start >/dev/null 2>&1 || true
	elif [ -f {BIN_PATH} ]; then
		start-stop-daemon --start --background --make-pidfile \
			--pidfile /run/firecrab-guest-agent.openrc.pid \
			--exec /bin/sh -- {BIN_PATH} \
			>/dev/null 2>&1 || true
	fi
"#
    )
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use std::collections::BTreeMap;

    use crate::rootfs::{run_debugfs, specialize_guest, write_into_image};

    fn debugfs_cat(path: &Path, guest_path: &str) -> String {
        let output = Command::new("debugfs")
            .arg("-R")
            .arg(format!("cat {guest_path}"))
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Alpine-like layout: `/usr/local` without `sbin`, OpenRC only.
    #[test]
    fn install_openrc_metrics_agent_on_alpine_layout() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let rootfs = directory.path().join("rootfs.ext4");
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&rootfs)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success());
        for dir in [
            "/etc",
            "/etc/init.d",
            "/etc/runlevels",
            "/etc/runlevels/default",
            "/usr",
            "/usr/local",
        ] {
            run_debugfs(&rootfs, &format!("mkdir {dir}")).unwrap();
        }
        // Seed OpenRC network-ready so specialize rewrites it with a kick.
        write_into_image(
            &rootfs,
            "/etc/init.d/firecrab-network-ready",
            b"#!/sbin/openrc-run\nstart() { :; }\n",
        )
        .unwrap();

        specialize_guest(&rootfs, id, &BTreeMap::new()).unwrap();

        let agent = debugfs_cat(&rootfs, BIN_PATH);
        assert!(
            agent.contains("FIRECRAB_USAGE"),
            "agent script missing FIRECRAB_USAGE marker"
        );
        let openrc = debugfs_cat(&rootfs, OPENRC_PATH);
        assert!(
            openrc.contains("command_background=true"),
            "OpenRC service must run the agent as a long-lived process"
        );
        assert!(
            guest_path_exists(&rootfs, OPENRC_RUNLEVEL_PATH),
            "agent must be enabled in default runlevel"
        );
        let ready = debugfs_cat(&rootfs, "/etc/init.d/firecrab-network-ready");
        assert!(
            ready.contains("firecrab-guest-agent"),
            "Alpine network-ready must kick the metrics agent"
        );
    }

    /// systemd wants entry must be a real symlink.
    #[test]
    fn install_enables_systemd_metrics_agent_as_symlink() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let rootfs = directory.path().join("rootfs.ext4");
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&rootfs)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success());
        for dir in [
            "/etc",
            "/etc/systemd",
            "/etc/systemd/system",
            "/etc/systemd/system/multi-user.target.wants",
            "/usr",
            "/usr/local",
            "/usr/local/sbin",
        ] {
            run_debugfs(&rootfs, &format!("mkdir {dir}")).unwrap();
        }

        specialize_guest(&rootfs, id, &BTreeMap::new()).unwrap();

        assert!(guest_path_exists(&rootfs, BIN_PATH));
        assert!(guest_path_exists(&rootfs, UNIT_PATH));
        let stat = run_debugfs(&rootfs, &format!("stat {WANTS_PATH}")).unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&stat.stdout),
            String::from_utf8_lossy(&stat.stderr)
        );
        assert!(
            text.contains("Type: symlink") || text.contains("symlink"),
            "multi-user wants entry must be a symlink, got: {text}"
        );
    }

    #[test]
    fn network_ready_kicks_reference_agent_paths() {
        let systemd = network_ready_kick_systemd();
        assert!(systemd.contains(UNIT_PATH));
        assert!(systemd.contains(SERVICE_NAME));
        assert!(systemd.contains(BIN_PATH));
        let openrc = network_ready_kick_openrc();
        assert!(openrc.contains(OPENRC_PATH));
        assert!(openrc.contains(BIN_PATH));
    }
}
