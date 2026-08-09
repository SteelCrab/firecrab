//! Per-VM writable disk management: copies a verified template rootfs into a
//! generation-scoped file under `{vm}/disks/{generation}.ext4` on first
//! prepare, and grows it when capacity increases. Stop/start reuses the same
//! active generation file (`public-docs/storage.md`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;
use uuid::Uuid;

use crate::artifacts::VmArtifactPaths;

/// Default root directory for per-VM state (disk, config, console log).
const VMS_DIR: &str = "data/vms";

/// Failure modes for preparing or growing a VM's rootfs disk.
#[derive(Debug, Error)]
pub enum RootfsError {
    /// Couldn't create the VM's own directory.
    #[error("failed to create VM directory {path}: {source}")]
    CreateDirectory {
        /// The directory that couldn't be created.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't stat the rootfs file.
    #[error("failed to inspect rootfs at {path}: {source}")]
    Inspect {
        /// The rootfs path that couldn't be inspected.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't copy the template into the temporary file.
    #[error("failed to copy template rootfs into {path}: {source}")]
    Copy {
        /// The temporary file path the copy was writing to.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't rename the temporary file into its final location.
    #[error("failed to publish rootfs at {path}: {source}")]
    Publish {
        /// The final rootfs path that couldn't be published.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't `set_len` the rootfs file to the new target size.
    #[error("failed to extend rootfs file at {path}: {source}")]
    Extend {
        /// The rootfs path that couldn't be extended.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Couldn't spawn `e2fsck`/`resize2fs`.
    #[error("failed to run '{tool}' on rootfs at {path}: {source}")]
    ResizeTool {
        /// The rootfs path the tool was run against.
        path: PathBuf,
        /// Which tool failed to spawn (`e2fsck` or `resize2fs`).
        tool: &'static str,
        #[source]
        source: io::Error,
    },
    /// `e2fsck`/`resize2fs` ran but reported failure.
    #[error("'{tool}' reported a failure while resizing rootfs at {path}: {stderr}")]
    ResizeFailed {
        /// The rootfs path the tool was run against.
        path: PathBuf,
        /// Which tool failed (`e2fsck` or `resize2fs`).
        tool: &'static str,
        /// The tool's stderr output.
        stderr: String,
    },
    /// Couldn't spawn `e2fsck` to safely recover a guest filesystem before
    /// modifying it offline.
    #[error("failed to run e2fsck recovery on rootfs at {path}: {source}")]
    RecoveryTool {
        /// The rootfs path being recovered.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// `e2fsck -p` could not safely recover a guest filesystem. The disk is
    /// deliberately left untouched by specialization so the user's data can
    /// be recovered with an explicit manual repair.
    #[error("e2fsck could not safely recover rootfs at {path}: {detail}")]
    RecoveryFailed {
        /// The rootfs path that needs manual recovery.
        path: PathBuf,
        /// Combined diagnostic output from `e2fsck`.
        detail: String,
    },
    /// `debugfs` didn't confirm writing a file into the guest's rootfs.
    #[error("failed to specialize guest rootfs at {path}: {detail}")]
    Specialize {
        /// The rootfs path being specialized.
        path: PathBuf,
        /// Human-readable detail (usually debugfs's own stderr).
        detail: String,
    },
}

/// The default per-VM state root (`data/vms`).
pub fn default_vms_dir() -> PathBuf {
    PathBuf::from(VMS_DIR)
}

/// Copies the verified template into `{paths.disks}/{generation}.ext4`
/// atomically (temp → rename). An existing generation file is reused so
/// stop/start keeps guest data; then grows to `target_bytes` when needed.
pub fn prepare_rootfs(
    paths: &VmArtifactPaths,
    generation: Uuid,
    template: &mut File,
    target_bytes: u64,
) -> Result<PathBuf, RootfsError> {
    paths
        .ensure_directories()
        .map_err(|error| RootfsError::CreateDirectory {
            path: paths.dir.clone(),
            source: io::Error::other(error.to_string()),
        })?;

    let rootfs = paths.rootfs(generation);
    let freshly_created = match fs::metadata(&rootfs) {
        Ok(_) => false,
        Err(source) if source.kind() == io::ErrorKind::NotFound => true,
        Err(source) => {
            return Err(RootfsError::Inspect {
                path: rootfs,
                source,
            });
        }
    };

    if freshly_created {
        let tmp = paths.rootfs_tmp(generation);
        // Drop any leftover temp from a prior crash before publishing.
        let _ = fs::remove_file(&tmp);
        if let Err(error) = publish(template, &tmp, &rootfs) {
            let _ = fs::remove_file(&tmp);
            let _ = fs::remove_file(&rootfs);
            return Err(error);
        }
    }

    if let Err(error) = grow(&rootfs, target_bytes) {
        // A fresh copy that fails to grow is safe to discard (a retry just
        // re-copies); an existing disk's prior contents must survive a
        // failed resize attempt, so it is left in place on that path.
        if freshly_created {
            let _ = fs::remove_file(&rootfs);
            let _ = fs::remove_file(paths.rootfs_tmp(generation));
        }
        return Err(error);
    }
    Ok(rootfs)
}

/// Extends the disk file to `target_bytes` (no-op if it's already at least
/// that size — ext4 shrink isn't supported here) and grows the filesystem
/// to fill it, via the host's `e2fsprogs` tools.
fn grow(rootfs: &Path, target_bytes: u64) -> Result<(), RootfsError> {
    let current = fs::metadata(rootfs)
        .map_err(|source| RootfsError::Inspect {
            path: rootfs.to_owned(),
            source,
        })?
        .len();
    if target_bytes <= current {
        return Ok(());
    }

    let file = OpenOptions::new()
        .write(true)
        .open(rootfs)
        .map_err(|source| RootfsError::Extend {
            path: rootfs.to_owned(),
            source,
        })?;
    file.set_len(target_bytes)
        .map_err(|source| RootfsError::Extend {
            path: rootfs.to_owned(),
            source,
        })?;
    drop(file);

    let resized = run_resize_tool(rootfs, "e2fsck", &["-f", "-y"], |status| {
        // 0 = clean, 1 = errors corrected; anything higher is a real failure.
        status.code().is_some_and(|code| code <= 1)
    })
    .and_then(|()| run_resize_tool(rootfs, "resize2fs", &[], |status| status.success()));

    if resized.is_err() {
        // The filesystem inside wasn't actually grown, but the file's raw
        // length now is — restore it so a retry's no-op check above (which
        // only compares raw length) doesn't mistake this for an
        // already-grown disk and skip redoing e2fsck/resize2fs.
        if let Ok(file) = OpenOptions::new().write(true).open(rootfs) {
            let _ = file.set_len(current);
        }
    }
    resized
}

/// Runs `tool` against `rootfs` and maps its exit status through `accept`
/// (since a successful `e2fsck` run can still exit non-zero for "errors
/// corrected").
fn run_resize_tool(
    rootfs: &Path,
    tool: &'static str,
    args: &[&str],
    accept: impl Fn(&std::process::ExitStatus) -> bool,
) -> Result<(), RootfsError> {
    let output = Command::new(tool)
        .args(args)
        .arg(rootfs)
        .output()
        .map_err(|source| RootfsError::ResizeTool {
            path: rootfs.to_owned(),
            tool,
            source,
        })?;
    if accept(&output.status) {
        Ok(())
    } else {
        Err(RootfsError::ResizeFailed {
            path: rootfs.to_owned(),
            tool,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Paths this project always strips from a VM's own rootfs copy, best
/// effort — a distro that doesn't have a given path is a no-op, not a
/// failure. SSH host keys and the entropy seed would otherwise be
/// byte-identical across every VM cloned from the same template (a real
/// MITM risk for the host keys); any cached DHCP lease from the template
/// build is stale for this VM's own lease. Regenerating fresh versions on
/// first boot is the same thing cloud-init does for every AWS AMI clone.
const STRIP_PATHS: &[&str] = &[
    "/etc/ssh/ssh_host_rsa_key",
    "/etc/ssh/ssh_host_rsa_key.pub",
    "/etc/ssh/ssh_host_ecdsa_key",
    "/etc/ssh/ssh_host_ecdsa_key.pub",
    "/etc/ssh/ssh_host_ed25519_key",
    "/etc/ssh/ssh_host_ed25519_key.pub",
    "/var/lib/systemd/random-seed",
    "/var/lib/urandom/random-seed",
    "/var/lib/dhcp/dhclient.leases",
    "/var/lib/dhcpcd/dhcpcd-eth0.lease",
];

const FIRECRAB_MOTD: &str = r#"███████╗██╗██████╗ ███████╗ ██████╗██████╗  █████╗ ██████╗
██╔════╝██║██╔══██╗██╔════╝██╔════╝██╔══██╗██╔══██╗██╔══██╗
█████╗  ██║██████╔╝█████╗  ██║     ██████╔╝███████║██████╔╝
██╔══╝  ██║██╔══██╗██╔══╝  ██║     ██╔══██╗██╔══██║██╔══██╗
██║     ██║██║  ██║███████╗╚██████╗██║  ██║██║  ██║██████╔╝
╚═╝     ╚═╝╚═╝  ╚═╝╚══════╝ ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝

███╗   ███╗██████╗      ██████╗     ██████╗    ██╗
████╗ ████║╚════██╗    ██╔═████╗   ██╔═████╗  ███║
██╔████╔██║ █████╔╝    ██║██╔██║   ██║██╔██║  ╚██║
██║╚██╔╝██║██╔═══╝     ████╔╝██║   ████╔╝██║   ██║
██║ ╚═╝ ██║███████╗    ╚██████╔╝██╗╚██████╔╝██╗██║
╚═╝     ╚═╝╚══════╝     ╚═════╝ ╚═╝ ╚═════╝ ╚═╝╚═╝
"#;

/// Per-VM guest specialization: writes this VM's deterministic hostname
/// (see `firecrab_helper_protocol::network::guest_hostname`) into
/// `/etc/hostname`, installs the Firecrab `/etc/motd`, then strips
/// [`STRIP_PATHS`] — all directly on the VM's own rootfs file via `debugfs -w`
/// (no mount, no root needed). Before those offline writes, it runs `e2fsck
/// -p` so an abruptly stopped guest's ext4 journal is recovered using only
/// e2fsck's automatic safe repairs.
/// Idempotent: safe to call again against an already-specialized disk.
pub fn specialize_guest(rootfs: &Path, id: Uuid) -> Result<(), RootfsError> {
    recover_before_specialization(rootfs)?;

    let hostname = firecrab_helper_protocol::network::guest_hostname(id);
    write_into_image(rootfs, "/etc/hostname", format!("{hostname}\n").as_bytes())?;
    write_into_image(rootfs, "/etc/motd", FIRECRAB_MOTD.as_bytes())?;
    for path in STRIP_PATHS {
        remove_from_image(rootfs, path);
    }
    // Ubuntu templates historically enabled systemd-resolved's stub resolv.conf
    // without installing the package, so getent always failed while dig@gateway
    // worked. Rewrite the readiness script whenever the path already exists.
    patch_network_ready_script(rootfs)?;
    // Metrics Agent: guest OS CPU/mem samples for the dashboard (own module).
    crate::guest_agent::install(rootfs)?;
    Ok(())
}

/// Guest path for the Ubuntu/Rocky network readiness oneshot.
const NETWORK_READY_SCRIPT_PATH: &str = "/usr/local/sbin/firecrab-network-ready.sh";
/// Alpine OpenRC network readiness service (template-provided).
const NETWORK_READY_OPENRC_PATH: &str = "/etc/init.d/firecrab-network-ready";

/// Hardened readiness probe body (IPv4 + DNS). Metrics Agent kick is
/// prepended from [`crate::guest_agent`] so agent paths stay single-sourced.
const NETWORK_READY_PROBE: &str = r#"
ipv4=""
for _ in $(seq 1 15); do
  ipv4=$(ip -4 -o addr show scope global 2>/dev/null | \
    awk '$2 != "lo" { split($4, a, "/"); print a[1]; exit }')
  [ -n "$ipv4" ] && break
  sleep 1
done

if [ -z "$ipv4" ]; then
  echo "FIRECRAB_NETWORK_FAILED no-ipv4-address"
  exit 0
fi

gw=$(ip -4 route show default 2>/dev/null | awk '{print $3; exit}')

# Some images symlink resolv.conf to systemd-resolved's stub without shipping
# the package — /run/systemd/resolve/stub-resolv.conf never appears.
if [ -n "$gw" ] && [ ! -e /run/systemd/resolve/stub-resolv.conf ]; then
  if [ -L /etc/resolv.conf ] || [ ! -s /etc/resolv.conf ]; then
    rm -f /etc/resolv.conf
    printf 'nameserver %s\n' "$gw" > /etc/resolv.conf
  fi
fi

dns_ok() {
  getent hosts example.com >/dev/null 2>&1 && return 0
  if [ -n "$gw" ] && command -v dig >/dev/null 2>&1; then
    ans=$(dig +short +time=2 +tries=1 @"$gw" example.com A 2>/dev/null || true)
    [ -n "$ans" ] && return 0
  fi
  return 1
}

for _ in $(seq 1 15); do
  if dns_ok; then
    echo "FIRECRAB_NETWORK_READY $ipv4"
    exit 0
  fi
  sleep 1
done
echo "FIRECRAB_NETWORK_FAILED dns-unreachable"
"#;

/// Alpine OpenRC network-ready probe body (kick injected separately).
const NETWORK_READY_OPENRC_PROBE: &str = r#"
	ipv4=""
	for _ in $(seq 1 10); do
		ipv4=$(ip -4 -o addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
		[ -n "$ipv4" ] && break
		sleep 1
	done
	if [ -z "$ipv4" ]; then
		echo "FIRECRAB_NETWORK_FAILED no-ipv4-address" >/dev/console
	elif getent hosts example.com >/dev/null 2>&1; then
		echo "FIRECRAB_NETWORK_READY $ipv4" >/dev/console
	else
		echo "FIRECRAB_NETWORK_FAILED dns-unreachable" >/dev/console
	fi
"#;

fn network_ready_script() -> String {
    format!(
        "#!/bin/sh\nset -eu\n\n{}{}",
        crate::guest_agent::network_ready_kick_systemd(),
        NETWORK_READY_PROBE
    )
}

fn network_ready_openrc() -> String {
    format!(
        r#"#!/sbin/openrc-run
description="Firecrab network readiness sentinel"

depend() {{
	need net
	after dhcpcd
}}

start() {{
{}{}}}
"#,
        crate::guest_agent::network_ready_kick_openrc(),
        NETWORK_READY_OPENRC_PROBE
    )
}

/// Replaces the guest readiness script when the template already has one.
/// Ubuntu/Rocky: systemd oneshot script. Alpine: OpenRC init script.
fn patch_network_ready_script(rootfs: &Path) -> Result<(), RootfsError> {
    if guest_path_exists(rootfs, NETWORK_READY_SCRIPT_PATH) {
        write_into_image(
            rootfs,
            NETWORK_READY_SCRIPT_PATH,
            network_ready_script().as_bytes(),
        )?;
        // debugfs `write` creates regular files as 0664; the oneshot unit
        // invokes this path directly, so it must stay executable.
        set_guest_file_mode(rootfs, NETWORK_READY_SCRIPT_PATH, "0100755");
    }
    if guest_path_exists(rootfs, NETWORK_READY_OPENRC_PATH) {
        write_into_image(
            rootfs,
            NETWORK_READY_OPENRC_PATH,
            network_ready_openrc().as_bytes(),
        )?;
        set_guest_file_mode(rootfs, NETWORK_READY_OPENRC_PATH, "0100755");
    }
    Ok(())
}

/// Best-effort `chmod` inside the image. debugfs does not fail the host
/// process when the field write is refused, so callers treat this as soft.
pub(crate) fn set_guest_file_mode(rootfs: &Path, guest_path: &str, mode: &str) {
    let _ = run_debugfs(rootfs, &format!("set_inode_field {guest_path} mode {mode}"));
}

pub(crate) fn guest_path_exists(rootfs: &Path, guest_path: &str) -> bool {
    match run_debugfs(rootfs, &format!("stat {guest_path}")) {
        Ok(output) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            !text.contains("File not found") && !text.to_ascii_lowercase().contains("doesn't exist")
        }
        Err(_) => false,
    }
}

/// Recovers an ext4 journal before [`specialize_guest`] makes any
/// direct `debugfs -w` changes. `-p` deliberately limits e2fsck to its
/// automatic safe repairs; unlike `-y`, it does not make a destructive or
/// ambiguous repair decision on behalf of the VM owner.
pub(crate) fn recover_before_specialization(rootfs: &Path) -> Result<(), RootfsError> {
    let output = Command::new("e2fsck")
        .arg("-p")
        .arg(rootfs)
        .output()
        .map_err(|source| RootfsError::RecoveryTool {
            path: rootfs.to_owned(),
            source,
        })?;

    // e2fsck uses 1 to report that it safely corrected errors. Any other
    // non-zero status needs operator attention, so refuse to write further
    // into the image and leave its existing contents available for recovery.
    if output
        .status
        .code()
        .is_some_and(|code| code == 0 || code == 1)
    {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => format!("e2fsck exited with status {}", output.status),
    };
    Err(RootfsError::RecoveryFailed {
        path: rootfs.to_owned(),
        detail,
    })
}

/// Writes `content` as `guest_path` inside `rootfs`'s ext4 image.
/// `debugfs`'s own `write` command refuses to overwrite an existing file,
/// so any prior version is removed first — making this safe to call again
/// against an already-specialized disk.
///
/// `pub(crate)` (rather than private) so `handlers::bootstrap`'s own tests
/// can seed a fixture disk with the exact guest-side output paths a real
/// bootstrap script run would have left behind, the same reuse Task 3 gave
/// other single-file-scoped helpers.
pub(crate) fn write_into_image(
    rootfs: &Path,
    guest_path: &str,
    content: &[u8],
) -> Result<(), RootfsError> {
    let staging = rootfs.with_extension("specialize.tmp");
    fs::write(&staging, content).map_err(|source| RootfsError::Specialize {
        path: rootfs.to_owned(),
        detail: format!("failed to stage content for {guest_path}: {source}"),
    })?;
    remove_from_image(rootfs, guest_path);
    let output = run_debugfs(rootfs, &format!("write {} {guest_path}", staging.display()));
    let _ = fs::remove_file(&staging);
    let output = output?;

    // debugfs's own exit code is always 0 regardless of whether the -R
    // command actually succeeded, so success is confirmed positively from
    // stdout instead of trusting the exit status or absence of stderr.
    if String::from_utf8_lossy(&output.stdout).contains("Allocated inode") {
        Ok(())
    } else {
        Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!(
                "debugfs did not confirm writing {guest_path}: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        })
    }
}

/// Extracts `guest_path` from `rootfs`'s ext4 image to `dest` on the host,
/// without mounting — the read counterpart to [`write_into_image`]. Used to
/// pull a bootstrap builder's freshly-assembled target rootfs/kernel/initrd
/// files out of its own disk once the guest-side script has finished
/// building them (`handlers::bootstrap::package_bootstrap`). Works for
/// files of any size `debugfs`'s `dump` command supports — the filesystem
/// itself is the only real limit, unlike `write_into_image`'s small
/// identity files.
pub fn dump_from_image(rootfs: &Path, guest_path: &str, dest: &Path) -> Result<(), RootfsError> {
    let output = run_debugfs(rootfs, &format!("dump {guest_path} {}", dest.display()))?;

    // debugfs's own exit code doesn't reliably reflect whether `dump` found
    // the path (same caveat as `write_into_image`), so success is confirmed
    // positively: a real dump produces a non-empty file at `dest`.
    match fs::metadata(dest) {
        Ok(metadata) if metadata.len() > 0 => Ok(()),
        _ => {
            let _ = fs::remove_file(dest);
            Err(RootfsError::Specialize {
                path: rootfs.to_owned(),
                detail: format!(
                    "debugfs did not produce a non-empty {} for {guest_path}: {}",
                    dest.display(),
                    String::from_utf8_lossy(&output.stderr)
                ),
            })
        }
    }
}

/// Best-effort removal of `guest_path` from `rootfs`'s image. Whether the
/// path exists on this particular distro's template varies, and debugfs's
/// exit code can't distinguish "removed" from "wasn't there" from "failed"
/// regardless — so the outcome is deliberately never inspected.
pub(crate) fn remove_from_image(rootfs: &Path, guest_path: &str) {
    let _ = run_debugfs(rootfs, &format!("rm {guest_path}"));
}

/// Runs one `debugfs -w -R <command>` invocation against `rootfs`.
pub(crate) fn run_debugfs(
    rootfs: &Path,
    command: &str,
) -> Result<std::process::Output, RootfsError> {
    Command::new("debugfs")
        .arg("-w")
        .arg("-R")
        .arg(command)
        .arg(rootfs)
        .output()
        .map_err(|source| RootfsError::ResizeTool {
            path: rootfs.to_owned(),
            tool: "debugfs",
            source,
        })
}

/// Copies `template` into `tmp` and atomically renames it to `rootfs`.
fn publish(template: &mut File, tmp: &Path, rootfs: &Path) -> Result<(), RootfsError> {
    let copy_error = |source| RootfsError::Copy {
        path: tmp.to_owned(),
        source,
    };

    // The registry's hash verification shares the descriptor offset, so the
    // template handle arrives at EOF.
    template.seek(SeekFrom::Start(0)).map_err(copy_error)?;
    let mut out = File::create(tmp).map_err(copy_error)?;
    io::copy(template, &mut out).map_err(copy_error)?;
    out.sync_all().map_err(copy_error)?;

    fs::rename(tmp, rootfs).map_err(|source| RootfsError::Publish {
        path: rootfs.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    fn template_file(directory: &Path, content: &[u8]) -> File {
        let path = directory.join("template.ext4");
        fs::write(&path, content).unwrap();
        let mut file = File::open(&path).unwrap();
        // Match open_verified's post-hash cursor position.
        file.seek(SeekFrom::End(0)).unwrap();
        file
    }

    #[test]
    fn copies_template_into_place() {
        let directory = tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        let mut template = template_file(directory.path(), b"template-bytes");
        let generation = Uuid::new_v4();

        let rootfs = prepare_rootfs(
            &paths,
            generation,
            &mut template,
            "template-bytes".len() as u64,
        )
        .unwrap();

        assert_eq!(rootfs, paths.rootfs(generation));
        assert_eq!(fs::read(&rootfs).unwrap(), b"template-bytes");
        assert!(!paths.rootfs_tmp(generation).exists());
    }

    #[test]
    fn prepare_reports_an_unusable_artifact_directory() {
        let directory = tempdir().unwrap();
        let blocker = directory.path().join("vms");
        fs::write(&blocker, b"not a directory").unwrap();
        let paths = VmArtifactPaths::for_vm(&blocker, Uuid::new_v4());
        let mut template = template_file(directory.path(), b"template-bytes");

        let error = prepare_rootfs(&paths, Uuid::new_v4(), &mut template, 16).unwrap_err();
        assert!(
            matches!(error, RootfsError::CreateDirectory { ref path, .. } if *path == paths.dir),
            "{error}"
        );
    }

    /// A brand-new copy that can't be grown is discarded entirely — no
    /// half-sized disk and no temp file left for the next attempt.
    #[test]
    fn a_fresh_copy_that_fails_to_grow_is_removed() {
        let directory = tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        // Not an ext4 image, so the grow step's e2fsck fails.
        let mut template = template_file(directory.path(), b"not an ext4 filesystem");
        let generation = Uuid::new_v4();

        let error = prepare_rootfs(&paths, generation, &mut template, 8 * 1024 * 1024).unwrap_err();
        assert!(matches!(error, RootfsError::ResizeFailed { .. }), "{error}");
        assert!(!paths.rootfs(generation).exists());
        assert!(!paths.rootfs_tmp(generation).exists());
    }

    #[test]
    fn reuses_an_existing_rootfs_without_recopying() {
        let directory = tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        let mut template = template_file(directory.path(), b"fresh-template");
        let generation = Uuid::new_v4();
        paths.ensure_directories().unwrap();
        fs::write(paths.rootfs(generation), b"existing-disk").unwrap();

        let rootfs = prepare_rootfs(
            &paths,
            generation,
            &mut template,
            "existing-disk".len() as u64,
        )
        .unwrap();

        assert_eq!(fs::read(&rootfs).unwrap(), b"existing-disk");
    }

    #[test]
    fn failed_copy_leaves_no_tmp_file() {
        let directory = tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        let template_path = directory.path().join("template.ext4");
        let mut unreadable = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&template_path)
            .unwrap();
        unreadable.write_all(b"template-bytes").unwrap();
        let generation = Uuid::new_v4();

        let error = prepare_rootfs(&paths, generation, &mut unreadable, 0).unwrap_err();

        assert!(matches!(error, RootfsError::Copy { .. }));
        assert!(!paths.rootfs_tmp(generation).exists());
        assert!(!paths.rootfs(generation).exists());
    }

    #[test]
    fn stop_start_reuses_same_generation_file_and_inode() {
        use std::os::unix::fs::MetadataExt;

        let directory = tempdir().unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        let mut template = template_file(directory.path(), b"guest-data-v1");
        let generation = Uuid::new_v4();
        let first = prepare_rootfs(
            &paths,
            generation,
            &mut template,
            b"guest-data-v1".len() as u64,
        )
        .unwrap();
        let ino = fs::metadata(&first).unwrap().ino();

        let mut template2 = template_file(directory.path(), b"should-not-overwrite");
        let second = prepare_rootfs(
            &paths,
            generation,
            &mut template2,
            b"guest-data-v1".len() as u64,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::metadata(&second).unwrap().ino(), ino);
        assert_eq!(fs::read(&second).unwrap(), b"guest-data-v1");

        let rt1 = paths.create_runtime(Uuid::new_v4()).unwrap();
        let rt2 = paths.create_runtime(Uuid::new_v4()).unwrap();
        assert_ne!(rt1.dir, rt2.dir);
    }

    /// End-to-end proof that `grow` actually works against a real
    /// filesystem, not just a `set_len`'d blob of bytes: builds a genuine
    /// small ext4 image with `mkfs.ext4`, copies it through `prepare_rootfs`
    /// with a larger target size, and checks the resulting filesystem
    /// actually reports the grown capacity.
    fn ext4_capacity_bytes(path: &Path) -> u64 {
        let dumpe2fs = Command::new("dumpe2fs")
            .args(["-h"])
            .arg(path)
            .output()
            .unwrap();
        let info = String::from_utf8_lossy(&dumpe2fs.stdout);
        let block_count: u64 = info
            .lines()
            .find_map(|line| line.strip_prefix("Block count:"))
            .expect("dumpe2fs must report a block count")
            .trim()
            .parse()
            .unwrap();
        let block_size: u64 = info
            .lines()
            .find_map(|line| line.strip_prefix("Block size:"))
            .expect("dumpe2fs must report a block size")
            .trim()
            .parse()
            .unwrap();
        block_count * block_size
    }

    #[test]
    fn grows_a_real_ext4_filesystem_to_the_requested_size() {
        let directory = tempdir().unwrap();
        let template_path = directory.path().join("template.ext4");
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&template_path)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success(), "mkfs.ext4 failed");

        let mut template = File::open(&template_path).unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        let target_bytes = 32 * 1024 * 1024;

        let rootfs = prepare_rootfs(&paths, Uuid::new_v4(), &mut template, target_bytes).unwrap();

        assert_eq!(fs::metadata(&rootfs).unwrap().len(), target_bytes);
        assert_eq!(ext4_capacity_bytes(&rootfs), target_bytes);
    }

    /// A VM whose `diskGb` was edited upward after it already had a disk
    /// (`public-docs/storage.md`) needs the *next* `prepare_rootfs` call
    /// — the "reuse existing disk" path, not the "fresh copy" one — to
    /// actually grow it.
    #[test]
    fn growing_an_already_existing_disk_is_applied_on_the_next_call() {
        let directory = tempdir().unwrap();
        let template_path = directory.path().join("template.ext4");
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&template_path)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success(), "mkfs.ext4 failed");

        let mut template = File::open(&template_path).unwrap();
        let paths = VmArtifactPaths::for_vm(&directory.path().join("vms"), Uuid::new_v4());
        let generation = Uuid::new_v4();
        let initial_bytes = 8 * 1024 * 1024;
        let grown_bytes = 24 * 1024 * 1024;

        let first = prepare_rootfs(&paths, generation, &mut template, initial_bytes).unwrap();
        assert_eq!(fs::metadata(&first).unwrap().len(), initial_bytes);

        let second = prepare_rootfs(&paths, generation, &mut template, grown_bytes).unwrap();
        assert_eq!(second, first);
        assert_eq!(fs::metadata(&second).unwrap().len(), grown_bytes);
        assert_eq!(ext4_capacity_bytes(&second), grown_bytes);
    }

    /// If e2fsck/resize2fs fail after `set_len` has already extended the raw
    /// file, `grow`'s own no-op check (`target_bytes <= current`) must not
    /// be fooled by that larger raw length on a later retry — otherwise the
    /// retry silently skips resizing a filesystem that was never actually
    /// grown.
    #[test]
    fn failed_resize_restores_the_original_file_length_so_a_retry_redoes_it() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        // Not a real ext4 filesystem, so e2fsck fails outright (verified:
        // exit code 8) instead of silently "fixing" it into something
        // resize2fs would then accept.
        fs::write(&rootfs, b"not an ext4 filesystem, just some bytes").unwrap();
        let original_len = fs::metadata(&rootfs).unwrap().len();

        let error = grow(&rootfs, original_len + 8 * 1024 * 1024).unwrap_err();
        assert!(matches!(
            error,
            RootfsError::ResizeFailed { tool: "e2fsck", .. }
        ));
        assert_eq!(
            fs::metadata(&rootfs).unwrap().len(),
            original_len,
            "a failed resize must not leave the file permanently enlarged"
        );
    }

    /// A real small ext4 image with the directories a specialization run
    /// needs to already exist (a blank `mkfs.ext4` image has no `/etc`).
    fn real_rootfs_with_guest_dirs(path: &Path) {
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(path)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success(), "mkfs.ext4 failed");
        for dir in ["/etc", "/etc/ssh", "/var", "/var/lib", "/var/lib/systemd"] {
            let output = run_debugfs(path, &format!("mkdir {dir}")).unwrap();
            assert!(
                output.stderr.is_empty()
                    || !String::from_utf8_lossy(&output.stderr).contains("File not found"),
                "mkdir {dir} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn debugfs_cat(path: &Path, guest_path: &str) -> String {
        let output = Command::new("debugfs")
            .arg("-R")
            .arg(format!("cat {guest_path}"))
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn filesystem_state(path: &Path) -> String {
        let output = Command::new("debugfs")
            .arg("-R")
            .arg("show_super_stats")
            .arg(path)
            .output()
            .expect("debugfs must be installed for this test");
        assert!(
            output.status.success(),
            "debugfs could not inspect superblock: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().strip_prefix("Filesystem state:"))
            .expect("debugfs must report the filesystem state")
            .trim()
            .to_owned()
    }

    #[test]
    fn specialize_guest_writes_the_deterministic_hostname() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);

        specialize_guest(&rootfs, id).unwrap();

        let hostname = firecrab_helper_protocol::network::guest_hostname(id);
        assert_eq!(
            debugfs_cat(&rootfs, "/etc/hostname"),
            format!("{hostname}\n")
        );
    }

    #[test]
    fn specialize_guest_installs_the_firecrab_motd() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);

        specialize_guest(&rootfs, Uuid::new_v4()).unwrap();

        assert_eq!(debugfs_cat(&rootfs, "/etc/motd"), FIRECRAB_MOTD);
    }

    /// A Firecracker process can be killed before the guest has cleanly
    /// unmounted ext4. The next start must recover that state before its
    /// direct debugfs writes, rather than leaving the filesystem dirty (or
    /// relying on debugfs to handle journal recovery itself).
    #[test]
    fn specialize_guest_recovers_an_unclean_ext4_before_offline_writes() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);

        let output = run_debugfs(&rootfs, "set_super_value state 0").unwrap();
        assert!(
            output.status.success(),
            "failed to mark test rootfs unclean"
        );
        assert_eq!(filesystem_state(&rootfs), "not clean");

        specialize_guest(&rootfs, id).unwrap();

        let hostname = firecrab_helper_protocol::network::guest_hostname(id);
        assert_eq!(
            debugfs_cat(&rootfs, "/etc/hostname"),
            format!("{hostname}\n")
        );
        assert_eq!(
            filesystem_state(&rootfs),
            "clean",
            "e2fsck -p must finish recovery before debugfs writes"
        );
    }

    #[test]
    fn specialize_guest_strips_ssh_host_keys_and_random_seed() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);

        let seeded = fs::write(directory.path().join("key.tmp"), b"not-a-real-key").is_ok();
        assert!(seeded);
        for path in ["/etc/ssh/ssh_host_rsa_key", "/var/lib/systemd/random-seed"] {
            let staging = directory.path().join("seed.tmp");
            fs::write(&staging, b"template-shared-secret").unwrap();
            run_debugfs(&rootfs, &format!("write {} {path}", staging.display())).unwrap();
        }

        specialize_guest(&rootfs, id).unwrap();

        for path in ["/etc/ssh/ssh_host_rsa_key", "/var/lib/systemd/random-seed"] {
            // debugfs's "cat" prints nothing to stdout for a missing file
            // (the "File not found" message goes to stderr instead), so an
            // empty read confirms the strip actually removed it.
            assert_eq!(
                debugfs_cat(&rootfs, path),
                "",
                "{path} should have been stripped"
            );
        }
    }

    #[test]
    fn specialize_guest_is_idempotent() {
        let directory = tempdir().unwrap();
        let id = Uuid::new_v4();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);

        specialize_guest(&rootfs, id).unwrap();
        // debugfs's `write` refuses to overwrite an existing file — this
        // must still succeed the second time (e.g. a VM restarted against
        // an already-specialized disk).
        specialize_guest(&rootfs, id).unwrap();

        let hostname = firecrab_helper_protocol::network::guest_hostname(id);
        assert_eq!(
            debugfs_cat(&rootfs, "/etc/hostname"),
            format!("{hostname}\n")
        );
    }

    #[test]
    fn specialize_guest_tolerates_a_distro_missing_some_strip_paths() {
        // Alpine has no /etc/machine-id or systemd random-seed — none of
        // STRIP_PATHS existing on a given template must not be an error.
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
        run_debugfs(&rootfs, "mkdir /etc").unwrap();

        specialize_guest(&rootfs, id).unwrap();
    }

    #[test]
    fn dump_from_image_extracts_a_file_written_earlier() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);
        write_into_image(&rootfs, "/etc/payload", b"hello from the guest disk\n").unwrap();

        let dest_dir = tempdir().unwrap();
        let dest = dest_dir.path().join("payload.out");
        dump_from_image(&rootfs, "/etc/payload", &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"hello from the guest disk\n");
    }

    #[test]
    fn dump_from_image_fails_clearly_for_a_missing_guest_path() {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        real_rootfs_with_guest_dirs(&rootfs);
        let dest_dir = tempdir().unwrap();
        let dest = dest_dir.path().join("missing.out");

        let error = dump_from_image(&rootfs, "/etc/does-not-exist", &dest);

        assert!(error.is_err());
    }
}
