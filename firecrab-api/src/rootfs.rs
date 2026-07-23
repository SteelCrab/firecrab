//! Per-VM writable disk management: copies a verified template rootfs into
//! `data/vms/{id}/rootfs.ext4` on first start, and grows it (raw file +
//! filesystem, via `e2fsck`/`resize2fs`) when the requested capacity exceeds
//! its current size.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;
use uuid::Uuid;

/// Default root directory for per-VM state (disk, config, console log).
const VMS_DIR: &str = "data/vms";
/// File name of a VM's published writable disk.
const ROOTFS_FILE_NAME: &str = "rootfs.ext4";
/// File name of the in-progress copy before it's renamed into place.
const ROOTFS_TMP_FILE_NAME: &str = "rootfs.ext4.tmp";

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

/// Path to a VM's writable rootfs disk file.
pub fn rootfs_path(vms_dir: &Path, id: Uuid) -> PathBuf {
    vms_dir.join(id.to_string()).join(ROOTFS_FILE_NAME)
}

/// Copies the verified template rootfs into the VM's writable disk at
/// `{vms_dir}/{id}/rootfs.ext4` (an existing disk is reused as-is so a
/// stopped VM keeps its data across restarts), then grows it to
/// `target_bytes` if that's larger than its current size -- covering both a
/// fresh copy and a disk whose VM had its `diskGb` edited upward since the
/// last start.
pub fn prepare_rootfs(
    vms_dir: &Path,
    id: Uuid,
    template: &mut File,
    target_bytes: u64,
) -> Result<PathBuf, RootfsError> {
    let vm_dir = vms_dir.join(id.to_string());
    let rootfs = rootfs_path(vms_dir, id);
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
        fs::create_dir_all(&vm_dir).map_err(|source| RootfsError::CreateDirectory {
            path: vm_dir.clone(),
            source,
        })?;

        let tmp = vm_dir.join(ROOTFS_TMP_FILE_NAME);
        if let Err(error) = publish(template, &tmp, &rootfs) {
            let _ = fs::remove_file(&tmp);
            return Err(error);
        }
    }

    if let Err(error) = grow(&rootfs, target_bytes) {
        // A fresh copy that fails to grow is safe to discard (a retry just
        // re-copies); an existing disk's prior contents must survive a
        // failed resize attempt, so it is left in place on that path.
        if freshly_created {
            let _ = fs::remove_file(&rootfs);
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

/// Per-VM guest specialization: writes this VM's deterministic hostname
/// (see `firecrab_helper_protocol::network::guest_hostname`) into
/// `/etc/hostname`, then strips [`STRIP_PATHS`] — all directly on the VM's
/// own rootfs file via `debugfs -w` (no mount, no root needed; the same
/// style already used for `e2fsck`/`resize2fs` in [`grow`]). Idempotent:
/// safe to call again against an already-specialized disk.
pub fn specialize_guest(vms_dir: &Path, id: Uuid) -> Result<(), RootfsError> {
    let rootfs = rootfs_path(vms_dir, id);
    let hostname = firecrab_helper_protocol::network::guest_hostname(id);
    write_into_image(&rootfs, "/etc/hostname", format!("{hostname}\n").as_bytes())?;
    for path in STRIP_PATHS {
        remove_from_image(&rootfs, path);
    }
    Ok(())
}

/// Writes `content` as `guest_path` inside `rootfs`'s ext4 image.
/// `debugfs`'s own `write` command refuses to overwrite an existing file,
/// so any prior version is removed first — making this safe to call again
/// against an already-specialized disk.
fn write_into_image(rootfs: &Path, guest_path: &str, content: &[u8]) -> Result<(), RootfsError> {
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

/// Best-effort removal of `guest_path` from `rootfs`'s image. Whether the
/// path exists on this particular distro's template varies, and debugfs's
/// exit code can't distinguish "removed" from "wasn't there" from "failed"
/// regardless — so the outcome is deliberately never inspected.
fn remove_from_image(rootfs: &Path, guest_path: &str) {
    let _ = run_debugfs(rootfs, &format!("rm {guest_path}"));
}

/// Runs one `debugfs -w -R <command>` invocation against `rootfs`.
fn run_debugfs(rootfs: &Path, command: &str) -> Result<std::process::Output, RootfsError> {
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
        let vms_dir = directory.path().join("vms");
        let mut template = template_file(directory.path(), b"template-bytes");
        let id = Uuid::new_v4();

        let rootfs =
            prepare_rootfs(&vms_dir, id, &mut template, "template-bytes".len() as u64).unwrap();

        assert_eq!(rootfs, vms_dir.join(id.to_string()).join("rootfs.ext4"));
        assert_eq!(fs::read(&rootfs).unwrap(), b"template-bytes");
        assert!(
            !vms_dir
                .join(id.to_string())
                .join("rootfs.ext4.tmp")
                .exists()
        );
    }

    #[test]
    fn reuses_an_existing_rootfs_without_recopying() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let mut template = template_file(directory.path(), b"fresh-template");
        let id = Uuid::new_v4();
        let vm_dir = vms_dir.join(id.to_string());
        fs::create_dir_all(&vm_dir).unwrap();
        fs::write(vm_dir.join("rootfs.ext4"), b"existing-disk").unwrap();

        let rootfs =
            prepare_rootfs(&vms_dir, id, &mut template, "existing-disk".len() as u64).unwrap();

        assert_eq!(fs::read(&rootfs).unwrap(), b"existing-disk");
    }

    #[test]
    fn failed_copy_leaves_no_tmp_file() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let template_path = directory.path().join("template.ext4");
        let mut unreadable = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&template_path)
            .unwrap();
        unreadable.write_all(b"template-bytes").unwrap();
        let id = Uuid::new_v4();

        let error = prepare_rootfs(&vms_dir, id, &mut unreadable, 0).unwrap_err();

        assert!(matches!(error, RootfsError::Copy { .. }));
        let vm_dir = vms_dir.join(id.to_string());
        assert!(!vm_dir.join("rootfs.ext4.tmp").exists());
        assert!(!vm_dir.join("rootfs.ext4").exists());
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
        let vms_dir = directory.path().join("vms");
        let id = Uuid::new_v4();
        let target_bytes = 32 * 1024 * 1024;

        let rootfs = prepare_rootfs(&vms_dir, id, &mut template, target_bytes).unwrap();

        assert_eq!(fs::metadata(&rootfs).unwrap().len(), target_bytes);
        assert_eq!(ext4_capacity_bytes(&rootfs), target_bytes);
    }

    /// A VM whose `diskGb` was edited upward after it already had a disk
    /// (`task-vm-resource-update.md`) needs the *next* `prepare_rootfs` call
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
        let vms_dir = directory.path().join("vms");
        let id = Uuid::new_v4();
        let initial_bytes = 8 * 1024 * 1024;
        let grown_bytes = 24 * 1024 * 1024;

        let first = prepare_rootfs(&vms_dir, id, &mut template, initial_bytes).unwrap();
        assert_eq!(fs::metadata(&first).unwrap().len(), initial_bytes);

        let second = prepare_rootfs(&vms_dir, id, &mut template, grown_bytes).unwrap();
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

    #[test]
    fn specialize_guest_writes_the_deterministic_hostname() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let id = Uuid::new_v4();
        let rootfs = vms_dir.join(id.to_string()).join(ROOTFS_FILE_NAME);
        fs::create_dir_all(rootfs.parent().unwrap()).unwrap();
        real_rootfs_with_guest_dirs(&rootfs);

        specialize_guest(&vms_dir, id).unwrap();

        let hostname = firecrab_helper_protocol::network::guest_hostname(id);
        assert_eq!(
            debugfs_cat(&rootfs, "/etc/hostname"),
            format!("{hostname}\n")
        );
    }

    #[test]
    fn specialize_guest_strips_ssh_host_keys_and_random_seed() {
        let directory = tempdir().unwrap();
        let vms_dir = directory.path().join("vms");
        let id = Uuid::new_v4();
        let rootfs = vms_dir.join(id.to_string()).join(ROOTFS_FILE_NAME);
        fs::create_dir_all(rootfs.parent().unwrap()).unwrap();
        real_rootfs_with_guest_dirs(&rootfs);

        let seeded = fs::write(directory.path().join("key.tmp"), b"not-a-real-key").is_ok();
        assert!(seeded);
        for path in ["/etc/ssh/ssh_host_rsa_key", "/var/lib/systemd/random-seed"] {
            let staging = directory.path().join("seed.tmp");
            fs::write(&staging, b"template-shared-secret").unwrap();
            run_debugfs(&rootfs, &format!("write {} {path}", staging.display())).unwrap();
        }

        specialize_guest(&vms_dir, id).unwrap();

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
        let vms_dir = directory.path().join("vms");
        let id = Uuid::new_v4();
        let rootfs = vms_dir.join(id.to_string()).join(ROOTFS_FILE_NAME);
        fs::create_dir_all(rootfs.parent().unwrap()).unwrap();
        real_rootfs_with_guest_dirs(&rootfs);

        specialize_guest(&vms_dir, id).unwrap();
        // debugfs's `write` refuses to overwrite an existing file — this
        // must still succeed the second time (e.g. a VM restarted against
        // an already-specialized disk).
        specialize_guest(&vms_dir, id).unwrap();

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
        let vms_dir = directory.path().join("vms");
        let id = Uuid::new_v4();
        let rootfs = vms_dir.join(id.to_string()).join(ROOTFS_FILE_NAME);
        fs::create_dir_all(rootfs.parent().unwrap()).unwrap();
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&rootfs)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success());
        run_debugfs(&rootfs, "mkdir /etc").unwrap();

        specialize_guest(&vms_dir, id).unwrap();
    }
}
