//! Shell repository inject: write pinned guest scripts into a VM rootfs and
//! enable a boot oneshot that runs them after network-ready.
//!
//! Works across every built-in guest image layout:
//! - **Alpine** — OpenRC (`/etc/init.d` + `runlevels/default`); ash `/bin/sh`
//! - **Ubuntu / Rocky** — systemd (`multi-user.target.wants` symlink)
//!
//! Scripts should be **POSIX `/bin/sh`** for all-image portability. A leading
//! `#!/bin/bash` is honored when bash is present (Ubuntu/Rocky); on Alpine
//! (no bash) that shebang fails with a clear `FIRECRAB_SHELL_FAILED … no-bash`
//! marker instead of a cryptic syntax error.

use std::path::Path;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::rootfs::{
    RootfsError, guest_path_exists, remove_from_image, run_debugfs, set_guest_file_mode,
    write_into_image,
};

/// Max script body size accepted by the API (bytes). Under the 64 KiB JSON
/// request cap with room for wrappers.
pub const MAX_SHELL_CONTENT_BYTES: usize = 32 * 1024;

/// Enforced at compile time rather than by a test, so raising the limit past
/// the request cap cannot build at all.
const _: () = assert!(MAX_SHELL_CONTENT_BYTES < 64 * 1024);

/// Max shells pinned on one VM.
pub const MAX_SHELLS_PER_VM: usize = 8;

/// Guest directory for pinned revision scripts.
const SHELLS_DIR: &str = "/var/lib/firecrab/shells";
const RUNNER_PATH: &str = "/usr/local/sbin/firecrab-run-shells.sh";
const SYSTEMD_UNIT: &str = "/etc/systemd/system/firecrab-shells.service";
const SYSTEMD_WANTS: &str = "/etc/systemd/system/multi-user.target.wants/firecrab-shells.service";
const OPENRC_PATH: &str = "/etc/init.d/firecrab-shells";
const OPENRC_RUNLEVEL: &str = "/etc/runlevels/default/firecrab-shells";

/// One script to inject: ordered, content is the body run under its shebang
/// (or `/bin/sh` when none).
#[derive(Debug, Clone)]
pub struct ShellScript {
    /// Pinned revision id (used in the guest file name).
    pub revision_id: Uuid,
    /// Script body.
    pub content: String,
}

/// Hex SHA-256 of `content` (UTF-8 bytes).
pub fn content_sha256(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

/// Installs ordered shell scripts into `rootfs` and enables a boot oneshot.
/// Empty `scripts` still ensures prior shell files are cleared so a re-pin
/// with zero shells does not re-run stale content.
pub fn install(rootfs: &Path, scripts: &[ShellScript]) -> Result<(), RootfsError> {
    clear_previous_shells(rootfs);

    if scripts.is_empty() {
        return Ok(());
    }

    ensure_guest_dir(rootfs, "/var/lib")?;
    ensure_guest_dir(rootfs, "/var/lib/firecrab")?;
    ensure_guest_dir(rootfs, SHELLS_DIR)?;

    if !ensure_sbin(rootfs)? {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: "cannot install shells: /usr/local/sbin missing".into(),
        });
    }

    for (index, script) in scripts.iter().enumerate() {
        // Stable slot names so a later re-pin can clear previous files with
        // `remove_from_image` (debugfs has no directory listing).
        let guest_path = format!("{SHELLS_DIR}/{index:02}.sh");
        let stamped = stamp_script_body(script.revision_id, &script.content);
        write_into_image(rootfs, &guest_path, stamped.as_bytes())?;
        set_guest_file_mode(rootfs, &guest_path, "0100755");
    }

    write_into_image(rootfs, RUNNER_PATH, runner_script().as_bytes())?;
    set_guest_file_mode(rootfs, RUNNER_PATH, "0100755");

    // Ubuntu + Rocky: systemd oneshot enabled under multi-user.target.
    if guest_path_exists(rootfs, "/etc/systemd/system") {
        write_into_image(rootfs, SYSTEMD_UNIT, systemd_unit().as_bytes())?;
        if guest_path_exists(rootfs, "/etc/systemd/system/multi-user.target.wants") {
            ensure_symlink(rootfs, SYSTEMD_WANTS, SYSTEMD_UNIT)?;
        }
    }

    // Alpine (and any OpenRC image): enable in the default runlevel.
    // Ubuntu ships `/etc/init.d` without OpenRC runlevels — write only when
    // we can actually enable the service so we do not leave a dead unit.
    if guest_path_exists(rootfs, "/etc/init.d")
        && guest_path_exists(rootfs, "/etc/runlevels/default")
    {
        write_into_image(rootfs, OPENRC_PATH, openrc_service().as_bytes())?;
        set_guest_file_mode(rootfs, OPENRC_PATH, "0100755");
        ensure_symlink(rootfs, OPENRC_RUNLEVEL, OPENRC_PATH)?;
    }

    Ok(())
}

/// Keep a leading `#!` on line 1 so the guest kernel / our runner see the
/// real interpreter. Stamp the revision as a comment on the next line.
fn stamp_script_body(revision_id: Uuid, content: &str) -> String {
    let mut body = content.to_string();
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let stamp = format!("# firecrab-shell revision={revision_id}");
    if body.starts_with("#!")
        && let Some((shebang, rest)) = body.split_once('\n')
    {
        return format!("{shebang}\n{stamp}\n{rest}");
    }
    format!("{stamp}\n{body}")
}

fn runner_script() -> String {
    // Portable ash/dash/bash-compatible runner. Built without `format!` so
    // shell `${...}` braces are not confused with Rust placeholders.
    //
    // Markers go to stdout *and* `/dev/console` so:
    // - OpenRC (Alpine) serial log shows them (stdout)
    // - systemd oneshot with journal+console shows them (stdout)
    // - even if stdout is swallowed, /dev/console still hits Firecracker
    let script = r#"#!/bin/sh
# Firecrab Shell repository runner — all guest images (Alpine/Ubuntu/Rocky).
set -u
dir="__SHELLS_DIR__"

log() {
  # Dual-write: service journal/serial + direct console (Metrics Agent style).
  printf '%s\n' "$*"
  printf '%s\n' "$*" >/dev/console 2>/dev/null || true
}

if [ ! -d "$dir" ]; then
  log "FIRECRAB_SHELL_DONE none"
  exit 0
fi

# Alpine templates ship ash only. If any pinned script asks for bash, try
# `apk add bash` once (network-ready already ran) so Ubuntu-style #!/bin/bash
# scripts work without rebuilding the image.
any_bash=0
for f in "$dir"/*.sh; do
  [ -f "$f" ] || continue
  line1=$(sed -n '1p' "$f" 2>/dev/null || true)
  case "$line1" in
    *bash*) any_bash=1 ;;
  esac
done
if [ "$any_bash" -eq 1 ] && ! command -v bash >/dev/null 2>&1; then
  if command -v apk >/dev/null 2>&1; then
    log "FIRECRAB_SHELL_INFO installing bash (apk)"
    # Best-effort; isolated guests or missing repos leave bash absent.
    apk add --no-cache bash >/dev/console 2>&1 || true
  fi
fi

failed=0
for f in "$dir"/*.sh; do
  [ -f "$f" ] || continue
  base=$(basename "$f")
  interp="/bin/sh"
  need_bash=0
  line1=$(sed -n '1p' "$f" 2>/dev/null || true)
  case "$line1" in
    \#\!*)
      rest=${line1#\#!}
      while [ "${rest# }" != "$rest" ]; do rest=${rest# }; done
      first=${rest%% *}
      case "$first" in
        */env)
          rest2=${rest#"$first"}
          while [ "${rest2# }" != "$rest2" ]; do rest2=${rest2# }; done
          prog=${rest2%% *}
          case "$prog" in bash) need_bash=1 ;; esac
          if [ -n "$prog" ] && command -v "$prog" >/dev/null 2>&1; then
            interp=$(command -v "$prog")
          fi
          ;;
        *bash)
          need_bash=1
          if [ -x "$first" ]; then
            interp=$first
          elif command -v bash >/dev/null 2>&1; then
            interp=$(command -v bash)
          fi
          ;;
        *)
          if [ -n "$first" ] && [ -x "$first" ]; then
            interp=$first
          elif [ -n "$first" ] && command -v "$(basename "$first")" >/dev/null 2>&1; then
            interp=$(command -v "$(basename "$first")")
          fi
          ;;
      esac
      ;;
  esac
  # Still no bash after apk attempt: fall back to /bin/sh so simple scripts
  # (echo, pwd) still run; pure bashisms will fail with a normal shell error.
  if [ "$need_bash" -eq 1 ]; then
    case "$interp" in
      *bash) ;;
      *)
        log "FIRECRAB_SHELL_WARN $base no-bash-using-sh"
        interp="/bin/sh"
        ;;
    esac
  fi
  log "FIRECRAB_SHELL_START $base interp=$interp"
  # Capture script stdout/stderr then dual-write (journal may drop oneshot output).
  out="/run/firecrab-shell-$base.out"
  if "$interp" "$f" >"$out" 2>&1; then
    while IFS= read -r line || [ -n "$line" ]; do
      [ -n "$line" ] && log "$line"
    done <"$out" 2>/dev/null || true
    rm -f "$out" 2>/dev/null || true
    log "FIRECRAB_SHELL_OK $base"
  else
    rc=$?
    while IFS= read -r line || [ -n "$line" ]; do
      [ -n "$line" ] && log "$line"
    done <"$out" 2>/dev/null || true
    rm -f "$out" 2>/dev/null || true
    log "FIRECRAB_SHELL_FAILED $base $rc"
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  log "FIRECRAB_SHELL_DONE failed"
  exit 0
fi
log "FIRECRAB_SHELL_DONE ok"
exit 0
"#;
    script.replace("__SHELLS_DIR__", SHELLS_DIR)
}

fn systemd_unit() -> String {
    // journal+console: markers on Firecracker serial for Ubuntu/Rocky.
    // After firecrab-network-ready when that unit exists; still starts without it.
    format!(
        r#"[Unit]
Description=Firecrab Shell repository scripts
After=network-online.target network.target firecrab-network-ready.service
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/bin/sh {RUNNER_PATH}
RemainAfterExit=yes
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
"#
    )
}

fn openrc_service() -> String {
    // Alpine: run after net + firecrab-network-ready (when present).
    format!(
        r#"#!/sbin/openrc-run
description="Firecrab Shell repository scripts"

depend() {{
	need localmount
	after net firewall dhcpcd firecrab-network-ready
}}

start() {{
	ebegin "Running firecrab shell scripts"
	/bin/sh {RUNNER_PATH}
	eend 0
}}
"#
    )
}

fn clear_previous_shells(rootfs: &Path) {
    // Best-effort cleanup of prior inject artifacts.
    remove_from_image(rootfs, RUNNER_PATH);
    remove_from_image(rootfs, SYSTEMD_UNIT);
    remove_from_image(rootfs, SYSTEMD_WANTS);
    remove_from_image(rootfs, OPENRC_PATH);
    remove_from_image(rootfs, OPENRC_RUNLEVEL);
    for index in 0..MAX_SHELLS_PER_VM {
        remove_from_image(rootfs, &format!("{SHELLS_DIR}/{index:02}.sh"));
    }
}

fn ensure_guest_dir(rootfs: &Path, path: &str) -> Result<(), RootfsError> {
    if guest_path_exists(rootfs, path) {
        return Ok(());
    }
    let _ = run_debugfs(rootfs, &format!("mkdir {path}"));
    if !guest_path_exists(rootfs, path) {
        return Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("debugfs failed to create {path}"),
        });
    }
    Ok(())
}

fn ensure_sbin(rootfs: &Path) -> Result<bool, RootfsError> {
    if guest_path_exists(rootfs, "/usr/local/sbin") {
        return Ok(true);
    }
    if !guest_path_exists(rootfs, "/usr/local") {
        return Ok(false);
    }
    ensure_guest_dir(rootfs, "/usr/local/sbin")?;
    Ok(true)
}

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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use tempfile::tempdir;

    fn debugfs_cat(path: &Path, guest_path: &str) -> String {
        let output = Command::new("debugfs")
            .arg("-R")
            .arg(format!("cat {guest_path}"))
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn mk_ext4(size: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&rootfs)
            .arg(size)
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success());
        (directory, rootfs)
    }

    fn mkdir_all(rootfs: &Path, dirs: &[&str]) {
        for dir in dirs {
            run_debugfs(rootfs, &format!("mkdir {dir}")).unwrap();
        }
    }

    #[test]
    fn content_sha256_is_stable() {
        assert_eq!(
            content_sha256("hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn stamp_preserves_shebang_on_first_line() {
        let id = Uuid::nil();
        let out = stamp_script_body(id, "#!/bin/bash\necho hi\n");
        assert!(
            out.starts_with("#!/bin/bash\n"),
            "shebang must stay line 1, got: {out:?}"
        );
        assert!(out.contains("# firecrab-shell revision="));
        assert!(out.contains("echo hi\n"));
    }

    #[test]
    fn stamp_without_shebang_prefixes_comment() {
        let out = stamp_script_body(Uuid::nil(), "echo hi\n");
        assert!(out.starts_with("# firecrab-shell revision="));
        assert!(out.contains("echo hi\n"));
    }

    /// Alpine-like: OpenRC only, `/usr/local` without `sbin`.
    #[test]
    fn install_openrc_shells_on_alpine_layout() {
        let (_dir, rootfs) = mk_ext4("8M");
        mkdir_all(
            &rootfs,
            &[
                "/etc",
                "/etc/init.d",
                "/etc/runlevels",
                "/etc/runlevels/default",
                "/usr",
                "/usr/local",
                "/var",
                "/var/lib",
            ],
        );

        let rev = Uuid::new_v4();
        install(
            &rootfs,
            &[ShellScript {
                revision_id: rev,
                content: "#!/bin/sh\necho hello-alpine\n".into(),
            }],
        )
        .unwrap();

        assert!(guest_path_exists(&rootfs, RUNNER_PATH));
        assert!(guest_path_exists(&rootfs, OPENRC_PATH));
        assert!(guest_path_exists(&rootfs, OPENRC_RUNLEVEL));
        assert!(guest_path_exists(&rootfs, &format!("{SHELLS_DIR}/00.sh")));
        assert!(
            !guest_path_exists(&rootfs, SYSTEMD_UNIT),
            "Alpine layout must not get a systemd unit"
        );

        let body = debugfs_cat(&rootfs, &format!("{SHELLS_DIR}/00.sh"));
        assert!(
            body.starts_with("#!/bin/sh\n"),
            "shebang preserved: {body:?}"
        );
        assert!(body.contains(&format!("revision={rev}")));

        let openrc = debugfs_cat(&rootfs, OPENRC_PATH);
        assert!(openrc.contains("firecrab-network-ready"));
        assert!(openrc.contains(RUNNER_PATH));
    }

    /// Ubuntu/Rocky-like: systemd multi-user wants symlink.
    #[test]
    fn install_systemd_shells_on_ubuntu_layout() {
        let (_dir, rootfs) = mk_ext4("8M");
        mkdir_all(
            &rootfs,
            &[
                "/etc",
                "/etc/systemd",
                "/etc/systemd/system",
                "/etc/systemd/system/multi-user.target.wants",
                // Ubuntu also has /etc/init.d without OpenRC runlevels.
                "/etc/init.d",
                "/usr",
                "/usr/local",
                "/usr/local/sbin",
                "/var",
                "/var/lib",
            ],
        );

        install(
            &rootfs,
            &[ShellScript {
                revision_id: Uuid::new_v4(),
                content: "#!/bin/bash\necho hello-ubuntu\n".into(),
            }],
        )
        .unwrap();

        assert!(guest_path_exists(&rootfs, RUNNER_PATH));
        assert!(guest_path_exists(&rootfs, SYSTEMD_UNIT));
        assert!(
            !guest_path_exists(&rootfs, OPENRC_PATH),
            "without OpenRC runlevels, do not drop a dead init.d unit"
        );

        let unit = debugfs_cat(&rootfs, SYSTEMD_UNIT);
        assert!(unit.contains("journal+console"));
        assert!(unit.contains("firecrab-network-ready.service"));

        let stat = run_debugfs(&rootfs, &format!("stat {SYSTEMD_WANTS}")).unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&stat.stdout),
            String::from_utf8_lossy(&stat.stderr)
        );
        assert!(
            text.contains("Type: symlink") || text.contains("symlink"),
            "multi-user wants must be a symlink, got: {text}"
        );

        let body = debugfs_cat(&rootfs, &format!("{SHELLS_DIR}/00.sh"));
        assert!(body.starts_with("#!/bin/bash\n"), "got {body:?}");
    }

    #[test]
    fn install_empty_clears_previous() {
        let (_dir, rootfs) = mk_ext4("8M");
        mkdir_all(
            &rootfs,
            &[
                "/etc",
                "/etc/systemd",
                "/etc/systemd/system",
                "/etc/systemd/system/multi-user.target.wants",
                "/usr",
                "/usr/local",
                "/usr/local/sbin",
                "/var",
                "/var/lib",
            ],
        );
        install(
            &rootfs,
            &[ShellScript {
                revision_id: Uuid::new_v4(),
                content: "echo x\n".into(),
            }],
        )
        .unwrap();
        assert!(guest_path_exists(&rootfs, RUNNER_PATH));
        install(&rootfs, &[]).unwrap();
        assert!(!guest_path_exists(&rootfs, RUNNER_PATH));
        assert!(!guest_path_exists(&rootfs, SYSTEMD_UNIT));
    }

    #[test]
    fn runner_mentions_console_and_alpine_bash() {
        let r = runner_script();
        assert!(r.contains("/dev/console"));
        assert!(r.contains("apk add"));
        assert!(r.contains("no-bash-using-sh"));
        assert!(r.contains(SHELLS_DIR));
    }
}
