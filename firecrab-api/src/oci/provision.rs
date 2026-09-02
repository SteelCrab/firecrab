//! Gives a merged container tree the guest runtime a MicroVM needs to boot.
//!
//! A container rootfs fails three ways at once: the kernel finds no `/sbin/init`
//! and panics, nothing asks for a DHCP lease so the guest never gets an address,
//! and nothing prints the readiness sentinel the host waits 180 seconds for.
//! Firecrab's existing guest features cannot fill the gap — they install only
//! when systemd or OpenRC is already present and no-op otherwise, which is
//! exactly the container case.
//!
//! So this stage injects an init, a DHCP client, the readiness sentinel, and the
//! metrics agent, all from one static program. The image's own userland is left
//! alone: its entrypoint becomes an ordinary service under the injected init in
//! a later stage, never PID 1.

use super::*;

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::sync::atomic::{AtomicU8, Ordering};

/// Guest path the kernel execs when the command line carries no `init=`.
///
/// Owning this path is what lets an imported image boot on the stock
/// `root=/dev/vda rw` command line every template already uses.
const GUEST_INIT: &str = "/sbin/init";
/// Guest path of the injected toolbox program.
///
/// The last component **must** be `busybox`. busybox is a multi-call binary:
/// `argv[0]`'s basename is the applet. A name such as `firecrab-busybox` is
/// not an applet, so every inittab and shebang invocation prints
/// `firecrab-busybox: applet not found` and DHCP never runs. The path lives
/// under `/etc/firecrab/` so an image that already ships `/bin/busybox` or
/// `/sbin/busybox` keeps its own copy.
pub(crate) const GUEST_TOOLBOX: &str = "/etc/firecrab/busybox";
/// busybox init reads its job table from here.
const GUEST_INITTAB: &str = "/etc/inittab";
/// Boot script run once, before anything else the guest does.
const GUEST_BOOT_SCRIPT: &str = "/etc/firecrab/rc.boot";
/// Console wrapper used when the image has no agetty (MOTD + ash).
const GUEST_CONSOLE_SCRIPT: &str = "/etc/firecrab/rc.console";
/// util-linux getty, in the usual usr-merge locations.
pub(crate) const GUEST_AGETTY_CANDIDATES: &[&str] = &["/sbin/agetty", "/usr/sbin/agetty"];
/// Login shell for the serial console.
pub(crate) const GUEST_BASH_CANDIDATES: &[&str] = &["/bin/bash", "/usr/bin/bash"];
/// Welcome banner shown on the injected console (same text as catalog VMs).
const GUEST_MOTD: &str = "/etc/motd";
/// Lease hook busybox `udhcpc` calls to apply an address.
const GUEST_DHCP_SCRIPT: &str = "/etc/firecrab/dhcp.script";
/// Directory a later stage drops the image's translated entrypoint into.
const GUEST_SERVICES: &str = "/etc/firecrab/services.d";
/// Mount points a container tree may not carry.
///
/// `/dev` matters most: `CONFIG_DEVTMPFS_MOUNT=y` mounts it, but only onto a
/// directory that exists. Without it there is no `/dev/console` and the guest
/// boots dark, with no sentinel and no way to see why.
const GUEST_MOUNT_POINTS: &[(&str, u32)] = &[
    ("/proc", 0o755),
    ("/sys", 0o755),
    ("/dev", 0o755),
    ("/run", 0o755),
    ("/tmp", 0o1777),
];

/// Traversal budget when following an image's ancestor symbolic links.
const SYMLINK_HOP_LIMIT: usize = 40;
/// Injection is running and may still be cancelled.
const INJECT_ACTIVE: u8 = 0;
/// The caller stopped waiting before injection finished.
const INJECT_CANCELLED: u8 = 1;
/// Injection completed; the state no longer changes.
const INJECT_FINISHED: u8 = 2;

/// Shared cancellation state consulted between injection steps.
struct InjectControl {
    /// Current phase, shared with the caller's cancellation guard.
    state: std::sync::Arc<AtomicU8>,
    /// Tree reported by cancellation errors.
    tree: PathBuf,
}

impl InjectControl {
    /// Fails once the caller stopped waiting, so a long injection stops early.
    fn check(&self) -> Result<(), ResolveError> {
        if self.state.load(Ordering::Acquire) == INJECT_ACTIVE {
            Ok(())
        } else {
            Err(ResolveError::GuestInjectionCancelled {
                path: self.tree.clone(),
            })
        }
    }

    /// Records that injection finished and the tree is the caller's again.
    fn finish(&self) {
        self.state.store(INJECT_FINISHED, Ordering::Release);
    }
}

/// Marks an abandoned injection cancelled when the caller's future is dropped.
struct CancelInjectionOnDrop {
    /// Shared phase, flipped to cancelled while still armed.
    state: std::sync::Arc<AtomicU8>,
    /// Cleared once the blocking worker has returned.
    armed: bool,
}

impl Drop for CancelInjectionOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.state.compare_exchange(
                INJECT_ACTIVE,
                INJECT_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

/// Runs the blocking injection on the pool and cancels it if the caller leaves.
pub(super) async fn provision_merged_rootfs(
    rootfs: MergedRootfs,
    options: &GuestRuntimeOptions<'_>,
) -> Result<ProvisionedRootfs, ResolveError> {
    let toolbox = busybox::ensure_toolbox(options).await?;
    let fastfetch = fastfetch::ensure_fastfetch(options.image_root, options.architecture).await;
    inject_guest_runtime(rootfs, &toolbox, fastfetch.as_ref()).await
}

/// Injects one already-verified toolbox program into a merged tree.
///
/// Split from acquisition so the injection rules can be tested without a
/// registry, and so a caller that already holds a program never re-verifies it.
pub(super) async fn inject_with_toolbox(
    rootfs: MergedRootfs,
    toolbox: &ToolboxProgram,
) -> Result<ProvisionedRootfs, ResolveError> {
    inject_guest_runtime(rootfs, toolbox, None).await
}

/// Injects the toolbox and, when the tree can exec it, a host-supplied fastfetch.
pub(super) async fn inject_guest_runtime(
    rootfs: MergedRootfs,
    toolbox: &ToolboxProgram,
    fastfetch: Option<&FastfetchProgram>,
) -> Result<ProvisionedRootfs, ResolveError> {
    let tree = rootfs.path().to_owned();
    let state = std::sync::Arc::new(AtomicU8::new(INJECT_ACTIVE));
    let control = InjectControl {
        state: state.clone(),
        tree: tree.clone(),
    };
    let mut cancel_on_drop = CancelInjectionOnDrop { state, armed: true };
    let worker_toolbox = toolbox.clone();
    let worker_fastfetch = fastfetch.map(|program| program.path().to_owned());
    let result = tokio::task::spawn_blocking(move || {
        inject_blocking(
            &tree,
            &worker_toolbox,
            worker_fastfetch.as_deref(),
            &control,
        )
    })
    .await
    .map_err(|error| {
        injection_io(
            "join worker",
            rootfs.path().to_owned(),
            io::Error::other(error),
        )
    });
    cancel_on_drop.armed = false;
    result??;

    Ok(ProvisionedRootfs {
        path: rootfs.path().to_owned(),
        toolbox: toolbox.digest().clone(),
    })
}

/// Writes the guest runtime, unwinding exactly what it touched on failure.
fn inject_blocking(
    tree: &Path,
    toolbox: &ToolboxProgram,
    fastfetch: Option<&Path>,
    control: &InjectControl,
) -> Result<(), ResolveError> {
    let mut unwind = InjectedPaths::default();
    let result = (|| {
        control.check()?;
        for (mount_point, mode) in GUEST_MOUNT_POINTS {
            ensure_guest_directory(tree, mount_point, *mode, &mut unwind)?;
        }
        control.check()?;
        ensure_guest_directory(tree, GUEST_SERVICES, 0o755, &mut unwind)?;

        control.check()?;
        install_program(tree, GUEST_TOOLBOX, toolbox.path(), &mut unwind)?;
        install_symlink(tree, GUEST_INIT, GUEST_TOOLBOX, &mut unwind)?;
        install_toolbox_commands(tree, &mut unwind)?;
        if let Some(path) = fastfetch
            && first_existing(tree, fastfetch::GLIBC_LOADERS).is_some()
        {
            install_program(tree, fastfetch::GUEST_PATH, path, &mut unwind)?;
        }

        control.check()?;
        if let Some(shell) = first_existing(tree, GUEST_BASH_CANDIDATES) {
            set_root_shell(tree, &shell)?;
        }
        ensure_securetty(tree, &mut unwind)?;
        install_file(
            tree,
            GUEST_INITTAB,
            inittab(first_existing(tree, GUEST_AGETTY_CANDIDATES).as_deref()).as_bytes(),
            0o644,
            &mut unwind,
        )?;
        install_file(
            tree,
            GUEST_BOOT_SCRIPT,
            boot_script().as_bytes(),
            0o755,
            &mut unwind,
        )?;
        install_file(
            tree,
            GUEST_CONSOLE_SCRIPT,
            console_script().as_bytes(),
            0o755,
            &mut unwind,
        )?;
        install_file(
            tree,
            GUEST_MOTD,
            crate::rootfs::FIRECRAB_MOTD.as_bytes(),
            0o644,
            &mut unwind,
        )?;
        install_file(
            tree,
            GUEST_DHCP_SCRIPT,
            dhcp_script().as_bytes(),
            0o755,
            &mut unwind,
        )?;
        install_file(
            tree,
            crate::guest_ssh::GUEST_SSHD_SERVICE,
            crate::guest_ssh::sshd_service_script().as_bytes(),
            0o755,
            &mut unwind,
        )?;

        control.check()?;
        install_file(
            tree,
            crate::guest_agent::BIN_PATH,
            crate::guest_agent::AGENT_SCRIPT.as_bytes(),
            0o755,
            &mut unwind,
        )
    })();

    match result {
        Ok(()) => {
            unwind.restore_modes();
            control.finish();
            unwind.keep();
            Ok(())
        }
        Err(error) => {
            unwind.restore();
            Err(error)
        }
    }
}

/// Every path this stage created or replaced, so a failure can undo it.
///
/// The merged tree belongs to the caller and is expensive to rebuild, so a
/// failed injection leaves it exactly as it was rather than deleting it. The
/// path set is small and known, which keeps the unwind bounded.
#[derive(Default)]
struct InjectedPaths {
    /// Host paths created by this stage, newest first when unwound.
    created: Vec<PathBuf>,
    /// Paths moved aside as `(backup, original)` so they can be moved back.
    displaced: Vec<(PathBuf, PathBuf)>,
    /// Directories unlocked for a replace, as `(path, original mode)`.
    chmodded: Vec<(PathBuf, u32)>,
    /// Set once injection succeeded and nothing should be undone.
    kept: bool,
}

impl InjectedPaths {
    /// Records a path this stage brought into existence.
    fn created(&mut self, path: PathBuf) {
        self.created.push(path);
    }

    /// Records an image path moved aside to make room.
    fn displaced(&mut self, backup: PathBuf, original: PathBuf) {
        self.displaced.push((backup, original));
    }

    /// Records a directory whose mode was raised so a replace could proceed.
    fn chmodded(&mut self, path: PathBuf, mode: u32) {
        self.chmodded.push((path, mode));
    }

    /// Puts image directory modes back. Runs on success and on failure.
    fn restore_modes(&mut self) {
        for (path, mode) in self.chmodded.drain(..).rev() {
            if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(mode)) {
                tracing::warn!(
                    error = %error,
                    path = %path.display(),
                    "failed to restore a guest directory mode"
                );
            }
        }
    }

    /// Keeps everything: injection succeeded.
    fn keep(&mut self) {
        self.kept = true;
    }

    /// Removes what this stage added and puts back what it moved aside.
    fn restore(&mut self) {
        if self.kept {
            return;
        }
        for path in self.created.drain(..).rev() {
            let removed = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir(&path),
                Ok(_) => fs::remove_file(&path),
                Err(_) => Ok(()),
            };
            if let Err(error) = removed
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(
                    error = %error,
                    path = %path.display(),
                    "failed to unwind an injected guest path"
                );
            }
        }
        for (backup, original) in self.displaced.drain(..).rev() {
            if let Err(error) = fs::rename(&backup, &original) {
                tracing::warn!(
                    error = %error,
                    path = %original.display(),
                    "failed to restore a displaced image path"
                );
            }
        }
        self.restore_modes();
    }
}

impl Drop for InjectedPaths {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Resolves a guest path's parent inside the tree, creating what is missing.
///
/// Ancestor symbolic links are **followed**, not refused: usr-merged images
/// ship `/sbin` as a link to `usr/sbin`, so refusing them would reject Ubuntu,
/// Debian, and Fedora outright. Following is made safe by clamping — an
/// absolute target re-roots at the tree and `..` stops at the tree root — so
/// resolution provably cannot leave the tree no matter what the image planted.
///
/// The final component is deliberately not resolved. Callers replace it without
/// following it, so an image's `/sbin/init` pointing at systemd loses the link
/// rather than overwriting systemd itself.
fn resolve_guest_parent(
    tree: &Path,
    guest_path: &str,
    unwind: &mut InjectedPaths,
) -> Result<PathBuf, ResolveError> {
    let relative = guest_path.trim_start_matches('/');
    let (parent, _) = relative.rsplit_once('/').unwrap_or(("", relative));
    let mut pending: Vec<String> = parent
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect();
    pending.reverse();

    let mut resolved = PathBuf::new();
    let mut hops = 0_usize;
    while let Some(component) = pending.pop() {
        if component == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&component);
        let absolute = tree.join(&candidate);
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.file_type().is_dir() => resolved = candidate,
            Ok(metadata) if metadata.file_type().is_symlink() => {
                hops += 1;
                if hops > SYMLINK_HOP_LIMIT {
                    return Err(guest_path_unusable(
                        guest_path,
                        GuestPathViolation::SymlinkLoop {
                            limit: SYMLINK_HOP_LIMIT,
                        },
                    ));
                }
                let target = fs::read_link(&absolute).map_err(|source| {
                    injection_io("read ancestor link", absolute.clone(), source)
                })?;
                if target.is_absolute() {
                    resolved = PathBuf::new();
                }
                let mut spliced: Vec<String> = target
                    .components()
                    .filter_map(|component| match component {
                        std::path::Component::Normal(part) => {
                            Some(part.to_string_lossy().into_owned())
                        }
                        std::path::Component::ParentDir => Some("..".to_owned()),
                        _ => None,
                    })
                    .collect();
                spliced.reverse();
                pending.extend(spliced);
            }
            Ok(_) => {
                return Err(guest_path_unusable(
                    guest_path,
                    GuestPathViolation::NonDirectoryAncestor {
                        ancestor: candidate,
                    },
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_directory(&absolute, 0o755)?;
                unwind.created(absolute);
                resolved = candidate;
            }
            Err(source) => {
                return Err(injection_io("inspect ancestor", absolute, source));
            }
        }
    }
    Ok(tree.join(resolved))
}

/// Makes `destination`'s parent writable when the image left it 0555.
///
/// Oracle Linux ships `/usr/sbin` as `dr-xr-xr-x`. Replacing `/sbin/init`
/// (resolved through the usr-merge link) then fails with `EACCES`. The
/// original mode is recorded and restored after injection.
fn ensure_parent_writable(
    destination: &Path,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    let Some(parent) = destination.parent() else {
        return Ok(());
    };
    let metadata = match fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(injection_io(
                "inspect guest parent",
                parent.to_owned(),
                source,
            ));
        }
    };
    if !metadata.file_type().is_dir() {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    if mode & 0o200 != 0 {
        return Ok(());
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(mode | 0o200))
        .map_err(|source| injection_io("unlock guest parent", parent.to_owned(), source))?;
    unwind.chmodded(parent.to_owned(), mode);
    Ok(())
}

/// Creates one directory with an exact mode.
///
/// The mode is restated after creation because `DirBuilder` masks it with the
/// service's umask, which would quietly drop `/tmp`'s sticky bit.
fn create_directory(path: &Path, mode: u32) -> Result<(), ResolveError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(mode);
    builder
        .create(path)
        .map_err(|source| injection_io("create guest directory", path.to_owned(), source))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|source| injection_io("set guest directory mode", path.to_owned(), source))
}

/// Creates a guest directory, leaving an existing one alone.
fn ensure_guest_directory(
    tree: &Path,
    guest_path: &str,
    mode: u32,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    let parent = resolve_guest_parent(tree, guest_path, unwind)?;
    let name = guest_path.rsplit('/').next().unwrap_or_default();
    let destination = parent.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(guest_path_unusable(
            guest_path,
            GuestPathViolation::NonDirectoryAncestor {
                ancestor: PathBuf::from(guest_path.trim_start_matches('/')),
            },
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_directory(&destination, mode)?;
            unwind.created(destination);
            Ok(())
        }
        Err(source) => Err(injection_io("inspect guest directory", destination, source)),
    }
}

/// Clears whatever the image left at a final component, recording the move.
///
/// The existing entry is never followed. A regular file or symlink is moved
/// aside so a failed injection can restore it; a directory is refused, because
/// silently deleting an image's directory tree is not this stage's call.
fn displace_existing(
    destination: &Path,
    guest_path: &str,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_dir() => Err(guest_path_unusable(
            guest_path,
            GuestPathViolation::NonDirectoryAncestor {
                ancestor: PathBuf::from(guest_path.trim_start_matches('/')),
            },
        )),
        Ok(_) => {
            ensure_parent_writable(destination, unwind)?;
            let backup = destination.with_extension(format!("firecrab-{}", Uuid::new_v4()));
            fs::rename(destination, &backup).map_err(|source| {
                injection_io("displace image path", destination.to_owned(), source)
            })?;
            unwind.displaced(backup, destination.to_owned());
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(injection_io(
            "inspect guest path",
            destination.to_owned(),
            source,
        )),
    }
}

/// Writes one guest file, replacing whatever the image left in its place.
fn install_file(
    tree: &Path,
    guest_path: &str,
    contents: &[u8],
    mode: u32,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    let parent = resolve_guest_parent(tree, guest_path, unwind)?;
    let name = guest_path.rsplit('/').next().unwrap_or_default();
    let destination = parent.join(name);
    ensure_parent_writable(&destination, unwind)?;
    displace_existing(&destination, guest_path, unwind)?;
    // `create_new` with `O_NOFOLLOW` closes the window between the displace
    // above and this create: a symlink planted in between cannot be followed.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&destination)
        .map_err(|source| injection_io("create guest file", destination.clone(), source))?;
    unwind.created(destination.clone());
    file.write_all(contents)
        .map_err(|source| injection_io("write guest file", destination.clone(), source))?;
    // `OpenOptions::mode` is masked by the service's umask; restate it so an
    // injected script is executable regardless of how the service was started.
    fs::set_permissions(&destination, fs::Permissions::from_mode(mode))
        .map_err(|source| injection_io("set guest file mode", destination, source))
}

/// Copies the toolbox program into the tree as an executable.
fn install_program(
    tree: &Path,
    guest_path: &str,
    source_path: &Path,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    let contents = fs::read(source_path)
        .map_err(|source| injection_io("read toolbox program", source_path.to_owned(), source))?;
    install_file(tree, guest_path, &contents, 0o755, unwind)
}

/// Busybox applets exposed on PATH when the image did not ship them.
pub(crate) const PATH_APPLETS: &[&str] = &[
    "ping",
    "ping6",
    "traceroute",
    "wget",
    "nc",
    "nslookup",
    "vi",
    "sh",
];

/// Directories a login shell searches, in the usual Unix order.
pub(crate) const PATH_LOOKUP_DIRS: &[&str] = &[
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
];

/// True when `applet` is already a file or symlink on a typical PATH.
pub(crate) fn applet_on_path(exists: impl Fn(&str) -> bool, applet: &str) -> bool {
    PATH_LOOKUP_DIRS
        .iter()
        .any(|dir| exists(&format!("{dir}/{applet}")))
}

/// Guest path for a new applet. Prefer `/usr/bin` on usr-merged trees.
pub(crate) fn applet_link_path(usr_bin_exists: bool, applet: &str) -> String {
    if usr_bin_exists {
        format!("/usr/bin/{applet}")
    } else {
        format!("/bin/{applet}")
    }
}

/// Links missing PATH tools at the toolbox. Does not invent `sudo`.
fn install_toolbox_commands(tree: &Path, unwind: &mut InjectedPaths) -> Result<(), ResolveError> {
    let usr_bin = tree.join("usr/bin").is_dir() || tree.join("usr/bin").is_symlink();
    let exists = |guest: &str| {
        let host = tree.join(guest.trim_start_matches('/'));
        host.is_file() || host.is_symlink()
    };
    for applet in PATH_APPLETS {
        if applet_on_path(exists, applet) {
            continue;
        }
        let dest = applet_link_path(usr_bin, applet);
        install_symlink(tree, &dest, GUEST_TOOLBOX, unwind)?;
    }
    Ok(())
}

/// Links one guest path at another, replacing what the image left there.
fn install_symlink(
    tree: &Path,
    guest_path: &str,
    target: &str,
    unwind: &mut InjectedPaths,
) -> Result<(), ResolveError> {
    let parent = resolve_guest_parent(tree, guest_path, unwind)?;
    let name = guest_path.rsplit('/').next().unwrap_or_default();
    let destination = parent.join(name);
    ensure_parent_writable(&destination, unwind)?;
    displace_existing(&destination, guest_path, unwind)?;
    std::os::unix::fs::symlink(target, &destination)
        .map_err(|source| injection_io("create guest symlink", destination.clone(), source))?;
    unwind.created(destination);
    Ok(())
}

/// Wraps a filesystem failure with the injection step that hit it.
fn injection_io(operation: &'static str, path: PathBuf, source: io::Error) -> ResolveError {
    ResolveError::GuestInjectionIo {
        operation,
        path,
        source,
    }
}

/// Wraps a rejection reason with the guest path it applies to.
fn guest_path_unusable(guest_path: &str, reason: GuestPathViolation) -> ResolveError {
    ResolveError::GuestPathUnusable {
        path: guest_path.to_owned(),
        reason,
    }
}

/// busybox init's job table.
///
/// Commands stay free of shell metacharacters on purpose: busybox init routes
/// any command containing them through `/bin/sh -c`, which a distroless image
/// does not have. When the image ships util-linux `agetty`, the serial
/// console is `ttyS0 → agetty → login → bash`. Otherwise the injected
/// wrapper still prints MOTD and drops into ash.
pub(crate) fn inittab(agetty: Option<&str>) -> String {
    let console = match agetty {
        Some(path) => format!(
            "ttyS0::respawn:{path} --autologin root --noclear --keep-baud 115200,57600,38400,9600 ttyS0 linux"
        ),
        None => format!("::respawn:-{GUEST_TOOLBOX} sh {GUEST_CONSOLE_SCRIPT}"),
    };
    format!(
        "# Firecrab guest runtime for an imported OCI image (public-docs/oci.md).\n\
         ::sysinit:{GUEST_TOOLBOX} sh {GUEST_BOOT_SCRIPT}\n\
         {console}\n\
         ::ctrlaltdel:{GUEST_TOOLBOX} poweroff -f\n\
         ::shutdown:{GUEST_TOOLBOX} sync\n\
         ::restart:{GUEST_INIT}\n"
    )
}

/// First path among `candidates` for which `exists` is true.
pub(crate) fn first_present<'a>(
    candidates: &[&'a str],
    exists: impl Fn(&str) -> bool,
) -> Option<&'a str> {
    candidates.iter().copied().find(|path| exists(path))
}

/// First existing regular file or symlink among `candidates`, as a guest path.
pub(crate) fn first_existing(tree: &Path, candidates: &[&str]) -> Option<String> {
    for path in candidates {
        let host = tree.join(path.trim_start_matches('/'));
        if host.is_file() || host.is_symlink() {
            return Some((*path).to_owned());
        }
    }
    None
}

/// Points root's login shell at `shell` so agetty/login start bash.
fn set_root_shell(tree: &Path, shell: &str) -> Result<(), ResolveError> {
    let path = tree.join("etc/passwd");
    let current = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            String::from("root:x:0:0:root:/root:/bin/sh\n")
        }
        Err(source) => return Err(injection_io("read guest passwd", path, source)),
    };
    let rewritten = rewrite_root_shell(&current, shell);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|source| injection_io("create guest etc", parent.to_owned(), source))?;
    }
    fs::write(&path, rewritten).map_err(|source| injection_io("write guest passwd", path, source))
}

/// Ensures `/etc/securetty` lists ttyS0 so root may log in on the serial console.
fn ensure_securetty(tree: &Path, unwind: &mut InjectedPaths) -> Result<(), ResolveError> {
    let path = tree.join("etc/securetty");
    match fs::read_to_string(&path) {
        Ok(text) if text.lines().any(|line| line.trim() == "ttyS0") => Ok(()),
        Ok(text) => {
            let mut next = text;
            if !next.ends_with('\n') && !next.is_empty() {
                next.push('\n');
            }
            next.push_str("ttyS0\n");
            fs::write(&path, next).map_err(|source| injection_io("update securetty", path, source))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            install_file(tree, "/etc/securetty", b"ttyS0\n", 0o644, unwind)
        }
        Err(source) => Err(injection_io("read securetty", path, source)),
    }
}

/// Replaces the shell field on the root line of an `/etc/passwd` body.
pub(crate) fn rewrite_root_shell(passwd: &str, shell: &str) -> String {
    let mut out = String::new();
    let mut seen_root = false;
    for line in passwd.lines() {
        if !seen_root && line.starts_with("root:") {
            seen_root = true;
            let mut fields: Vec<&str> = line.split(':').collect();
            if fields.len() >= 7 {
                fields[6] = shell;
                out.push_str(&fields.join(":"));
            } else {
                out.push_str(&format!("root:x:0:0:root:/root:{shell}"));
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !seen_root {
        out.push_str(&format!("root:x:0:0:root:/root:{shell}\n"));
    }
    out
}

/// Everything between the kernel handing off and the host seeing a sentinel.
pub(crate) fn boot_script() -> String {
    format!(
        r#"#!{GUEST_TOOLBOX} sh
# Firecrab guest runtime for an imported OCI image (public-docs/oci.md).
# Avoid `set -e`: one failed mount must not leave the host waiting out the full
# readiness timeout for a sentinel that will now never be printed.
BB={GUEST_TOOLBOX}

$BB mount -t proc -o nosuid,nodev,noexec proc /proc 2>/dev/null
$BB mount -t sysfs -o nosuid,nodev,noexec sysfs /sys 2>/dev/null
$BB mount -t devtmpfs devtmpfs /dev 2>/dev/null
$BB mkdir -p /dev/pts
$BB mount -t devpts -o nosuid,noexec devpts /dev/pts 2>/dev/null
$BB mount -t tmpfs -o nosuid,nodev,mode=755 tmpfs /run 2>/dev/null
# Official images expect the Linux fd nodes Docker always provides.
# Bash process substitution (initdb, entrypoints) opens /dev/fd/N.
[ -e /dev/fd ] || $BB ln -sf /proc/self/fd /dev/fd
[ -e /dev/stdin ] || $BB ln -sf /proc/self/fd/0 /dev/stdin
[ -e /dev/stdout ] || $BB ln -sf /proc/self/fd/1 /dev/stdout
[ -e /dev/stderr ] || $BB ln -sf /proc/self/fd/2 /dev/stderr

# specialize_guest writes /etc/hostname; systemd would apply it, busybox init does not.
if [ -s /etc/hostname ]; then
  $BB hostname -F /etc/hostname 2>/dev/null
  $BB cat /etc/hostname > /proc/sys/kernel/hostname 2>/dev/null
fi

# UTF-8 so CJK typed on the serial console is not treated as POSIX C.
export LANG="${{LANG:-C.UTF-8}}"
export LC_ALL="$LANG" LC_CTYPE="$LANG"
$BB stty iutf8 2>/dev/null

# Metrics first, so the dashboard has samples even when the network fails.
$BB setsid $BB sh {agent} >/dev/null 2>&1 &

$BB ip link set lo up 2>/dev/null
# A bare udhcpc on an administratively down link never sends a single packet and
# hangs forever, so the link comes up first, always.
if ! $BB ip link set eth0 up 2>/dev/null; then
  echo "FIRECRAB_NETWORK_FAILED no-ipv4-address" >/dev/console
  exit 0
fi

$BB udhcpc -i eth0 -n -q -t 8 -T 2 -s {dhcp} >/dev/console 2>&1

ipv4=""
tries=0
while [ "$tries" -lt 15 ]; do
  ipv4=$($BB ip -4 -o addr show eth0 2>/dev/null | $BB awk '{{print $4; exit}}' | $BB cut -d/ -f1)
  [ -n "$ipv4" ] && break
  tries=$((tries + 1))
  $BB sleep 1
done

if [ -z "$ipv4" ]; then
  echo "FIRECRAB_NETWORK_FAILED no-ipv4-address" >/dev/console
  exit 0
fi

dns_ok=0
tries=0
while [ "$tries" -lt 15 ]; do
  if $BB nslookup example.com >/dev/null 2>&1; then
    dns_ok=1
    break
  fi
  tries=$((tries + 1))
  $BB sleep 1
done

if [ "$dns_ok" -ne 1 ]; then
  echo "FIRECRAB_NETWORK_FAILED dns-unreachable" >/dev/console
  exit 0
fi

echo "FIRECRAB_NETWORK_READY $ipv4" >/dev/console

{base_packages}

# Fallback only: glibc guests already received a pinned /usr/bin/fastfetch at
# import. Alpine and other musl trees still try the guest package manager.
if [ ! -x /usr/bin/fastfetch ]; then
  if [ -x /usr/bin/apt-get ]; then
    DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get update -qq >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y -qq fastfetch >/dev/null 2>&1
  elif [ -x /usr/bin/dnf ]; then
    /usr/bin/dnf install -y -q fastfetch >/dev/null 2>&1
  elif [ -x /sbin/apk ]; then
    /sbin/apk add --no-cache fastfetch >/dev/null 2>&1
  fi
fi

# A later import stage translates the image entrypoint into a program here.
$BB mkdir -p /run/firecrab
for service in {services}/*; do
  [ -x "$service" ] || continue
  name=${{service##*/}}
  $BB setsid "$service" >/dev/console 2>&1 &
  echo $! > /run/firecrab/$name.pid
  [ "$name" = app ] && echo $! > /run/firecrab-app.pid
done
exit 0
"#,
        agent = crate::guest_agent::BIN_PATH,
        dhcp = GUEST_DHCP_SCRIPT,
        services = GUEST_SERVICES,
        base_packages = BASE_PACKAGE_INSTALL,
    )
}

/// First-boot install of a small operator set. Slim OCI images ship a
/// package manager and empty lists, so `apt-get install ping` fails until
/// `update` has run. A failed attempt leaves no stamp and retries next boot.
/// Distroless trees have no manager; the busybox applets are enough.
pub(crate) const BASE_PACKAGE_INSTALL: &str = r#"
# First boot only. Container images rarely ship ping/curl.
if [ ! -f /etc/firecrab/base-packages.ok ]; then
  ok=0
  if [ -x /usr/bin/apt-get ]; then
    if DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get update -qq \
      && DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y -qq \
        iputils-ping iproute2 ca-certificates curl procps openssh-server udev; then
      ok=1
    fi
  elif [ -x /usr/bin/dnf ]; then
    /usr/bin/dnf install -y -q iputils iproute ca-certificates curl procps-ng openssh-server && ok=1
  elif [ -x /usr/bin/microdnf ]; then
    /usr/bin/microdnf -y install iputils iproute ca-certificates curl procps-ng openssh-server && ok=1
  elif [ -x /usr/bin/yum ]; then
    /usr/bin/yum install -y -q iputils iproute ca-certificates curl procps-ng openssh-server && ok=1
  elif [ -x /sbin/apk ]; then
    /sbin/apk add --no-cache iputils iproute2 ca-certificates curl procps openssh && ok=1
  elif [ -x /usr/bin/apk ]; then
    /usr/bin/apk add --no-cache iputils iproute2 ca-certificates curl procps openssh && ok=1
  elif [ -x /usr/bin/zypper ]; then
    /usr/bin/zypper --non-interactive install -y iputils iproute2 ca-certificates curl procps openssh udev && ok=1
  elif [ -x /usr/bin/pacman ]; then
    /usr/bin/pacman -Sy --noconfirm --needed iputils iproute2 ca-certificates curl procps-ng openssh && ok=1
  else
    ok=1
  fi
  [ "$ok" -eq 1 ] && $BB touch /etc/firecrab/base-packages.ok
fi
"#;

/// Interactive console: MOTD, fastfetch when present, then ash.
pub(crate) fn console_script() -> String {
    format!(
        r#"#!{GUEST_TOOLBOX} sh
# Firecrab injected console (public-docs/oci.md).
BB={GUEST_TOOLBOX}
PATH="/usr/local/bin:/usr/local/sbin:$PATH"
export PATH
export LANG="${{LANG:-C.UTF-8}}"
export LC_ALL="$LANG" LC_CTYPE="$LANG"
$BB stty iutf8 2>/dev/null
if [ -s /etc/hostname ]; then
  $BB hostname -F /etc/hostname 2>/dev/null
  $BB cat /etc/hostname > /proc/sys/kernel/hostname 2>/dev/null
fi
[ -s /etc/motd ] && $BB cat /etc/motd
if [ -x /usr/bin/fastfetch ]; then
  /usr/bin/fastfetch
elif [ -x /usr/bin/neofetch ]; then
  /usr/bin/neofetch
fi
exec $BB sh
"#
    )
}

/// busybox `udhcpc` lease hook.
///
/// The lease arrives in the environment, not in arguments. `mask` is a prefix
/// length in busybox, unlike the dotted `subnet` beside it.
fn dhcp_script() -> String {
    format!(
        r#"#!{GUEST_TOOLBOX} sh
# Firecrab guest DHCP hook for an imported OCI image (public-docs/oci.md).
BB={GUEST_TOOLBOX}

case "$1" in
  deconfig)
    $BB ip addr flush dev "$interface" 2>/dev/null
    $BB ip link set "$interface" up 2>/dev/null
    ;;
  bound|renew)
    $BB ip addr flush dev "$interface" 2>/dev/null
    $BB ip addr add "$ip/${{mask:-24}}" dev "$interface" 2>/dev/null
    $BB ip link set "$interface" up 2>/dev/null
    for gateway in $router; do
      $BB ip route add default via "$gateway" dev "$interface" 2>/dev/null
      break
    done
    # Some images symlink resolv.conf at a resolver stub they never ship, so
    # the link target never appears. Replace the link with a real file.
    [ -L /etc/resolv.conf ] && $BB rm -f /etc/resolv.conf
    : >/etc/resolv.conf
    for server in $dns; do
      printf 'nameserver %s\n' "$server" >>/etc/resolv.conf
    done
    ;;
esac
exit 0
"#
    )
}
