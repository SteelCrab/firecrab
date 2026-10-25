//! A linked worktree never inherits ownership of the primary management VM.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Ownership and backend port record stored in each private managed home.
const RECORD: &str = "dev-instance.json";
/// Authenticate the macOS child process selected by the parent dispatcher.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const CHILD: &str = "FIRECRAB_DEV_INSTANCE_ID";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Persist one checkout's backend identity independently of its current branch.
pub struct Instance {
    /// Stable lowercase hash prefix of the checkout's canonical Git directory.
    pub id: String,
    /// Canonical checkout root; retained in records after the worktree is removed.
    pub source: PathBuf,
    /// Canonical per-worktree Git metadata directory used to derive `id`.
    pub git_dir: PathBuf,
    /// Protected primary base used only to derive sibling development paths.
    pub primary_home: PathBuf,
    /// Private runtime, disks, credentials, and ownership record directory.
    pub managed_home: PathBuf,
    /// Private host binary directory outside the primary VM's shared home.
    pub install_dir: PathBuf,
    /// Persisted loopback API port, distinct from all registered backend ports.
    pub api_port: u16,
    /// Persisted native resource port; Docker instances reserve it without exposing controls.
    pub resource_port: u16,
}

/// Report malformed or foreign state without permitting primary fallback.
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Reject existing symlink components before using a state path.
fn safe_path(path: &Path) -> io::Result<()> {
    super::super::managed_home::reject_symlink_components(path)
        .map_err(|error| invalid(error.to_string()))
}

/// Make a state path absolute and reject unsafe components before filesystem use.
fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid("development state paths must not contain '..'"));
    }
    let path = std::path::absolute(path)?;
    safe_path(&path)?;
    Ok(path)
}

/// Preserve removed worktree records while requiring every existing ancestor to be canonical.
fn canonical_or_missing(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid(
            "development source and Git directory must be absolute",
        ));
    }
    absolute(path)?;
    let mut existing = path;
    loop {
        match fs::canonicalize(existing) {
            Ok(canonical) if canonical == existing => return Ok(()),
            Ok(_) => {
                return Err(invalid(
                    "development source and Git directory must be canonical",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Accept only fixed-width lowercase hexadecimal instance directory names.
fn valid_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Recognize private layout paths whose missing record must never imply the primary home.
fn private_path(home: &Path) -> bool {
    home.file_name().is_some_and(|name| name == "micromanager")
        && home
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(valid_id)
        && home
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "dev-instances")
}

impl Instance {
    /// Check private layout, canonical identity paths, and backend port constraints.
    fn validate(&self, home: &Path) -> io::Result<()> {
        let root = self
            .primary_home
            .parent()
            .ok_or_else(|| invalid("primary home has no parent"))?;
        let expected = root.join("dev-instances").join(&self.id);
        if !valid_id(&self.id)
            || !self.primary_home.is_absolute()
            || self.managed_home != expected.join("micromanager")
            || self.install_dir != expected.join("bin")
            || self.managed_home != home
            || self.managed_home.starts_with(&self.primary_home)
            || self.api_port < 1024
            || self.resource_port < 1024
            || self.api_port == self.resource_port
            || [5523, 5524, 8080].contains(&self.api_port)
            || [5523, 5524, 8080].contains(&self.resource_port)
        {
            return Err(invalid(
                "invalid or foreign development instance; primary services were not selected",
            ));
        }
        for path in [&self.primary_home, &self.managed_home, &self.install_dir] {
            absolute(path)?;
        }
        for path in [&self.source, &self.git_dir] {
            canonical_or_missing(path)?;
        }
        Ok(())
    }

    /// Atomically create the ownership record without replacing an existing instance.
    fn save(&self) -> io::Result<()> {
        self.validate(&self.managed_home)?;
        private_directory(&self.managed_home)?;
        let mut staged = tempfile::NamedTempFile::new_in(&self.managed_home)?;
        staged.write_all(&serde_json::to_vec(self).map_err(|error| invalid(error.to_string()))?)?;
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(self.managed_home.join(RECORD))
            .map_err(|error| error.error)?;
        Ok(())
    }
}

/// Create state directories with user-only Unix permissions after checking their paths.
fn private_directory(path: &Path) -> io::Result<()> {
    safe_path(path)?;
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Validate one record; only an ordinary primary path may have no ownership file.
fn load_record(home: &Path) -> io::Result<Option<Instance>> {
    safe_path(home)?;
    let record = home.join(RECORD);
    safe_path(&record)?;
    let bytes = match fs::read(&record) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !private_path(home) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let instance: Instance =
        serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
    instance.validate(home)?;
    Ok(Some(instance))
}

/// Load selected ownership and reject conflicting backend reservations anywhere in its registry.
pub fn load(home: &Path) -> io::Result<Option<Instance>> {
    let state = load_record(home)?;
    if state.is_some() {
        let root = home
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("private registry path is missing"))?;
        reserved_ports(root)?;
    }
    Ok(state)
}

/// Protect primary and registered backend ports, including instances created after relay startup.
#[cfg(any(target_os = "macos", test))]
pub fn registered_backend_ports(home: &Path) -> io::Result<HashSet<u16>> {
    let home = absolute(home)?;
    let state = load_record(&home)?;
    let primary = state
        .as_ref()
        .map_or(&home, |instance| &instance.primary_home);
    let root = primary
        .parent()
        .ok_or_else(|| invalid("primary managed home has no parent"))?
        .join("dev-instances");
    safe_path(&root)?;
    if !root.exists() {
        return Ok(HashSet::from([5523, 5524, 8080]));
    }
    let lock_path = root.join("registry.lock");
    safe_path(&lock_path)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    // A creator holds the exclusive lock until its ownership record is complete.
    FileExt::lock_shared(&lock)?;
    reserved_ports(&root)
}

#[cfg(any(target_os = "macos", test))]
/// Select the private API port, or the existing primary port for an ordinary home.
pub fn api_port(home: &Path) -> io::Result<u16> {
    Ok(load(home)?.map_or(5523, |instance| instance.api_port))
}

/// Select the private native resource listener without silently accepting damaged private state.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub fn resource_port(home: &Path) -> io::Result<u16> {
    Ok(load(home)?.map_or(5524, |instance| instance.resource_port))
}

#[cfg(target_os = "macos")]
/// Build the selected host API base URL for native VM commands and forwarding.
pub fn api_url(home: &Path) -> io::Result<String> {
    Ok(format!("http://127.0.0.1:{}", api_port(home)?))
}

#[cfg(any(target_os = "macos", test))]
/// Give private launchd services a stable suffix while preserving the primary label.
pub fn label(home: &Path, base: &str) -> io::Result<String> {
    Ok(load(home)?.map_or_else(
        || base.to_owned(),
        |instance| format!("{base}.dev-{}", instance.id),
    ))
}

/// Canonical Git selection used before any backend command may run.
struct GitIdentity {
    /// Checkout root reported by Git and canonicalized by the host filesystem.
    source: PathBuf,
    /// Per-checkout metadata directory; differs from the common directory for linked worktrees.
    git_dir: PathBuf,
    /// Whether the checkout is linked to another checkout's common Git directory.
    linked: bool,
}

/// Keep canonical metadata, but pass Git paths without Windows verbatim prefixes.
#[cfg(windows)]
fn git_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    use std::ffi::OsString;
    use std::path::Prefix;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return std::borrow::Cow::Borrowed(path);
    };
    let mut ordinary = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut prefix = OsString::from(r"\\");
            prefix.push(server);
            prefix.push(r"\");
            prefix.push(share);
            PathBuf::from(prefix)
        }
        _ => return std::borrow::Cow::Borrowed(path),
    };
    ordinary.push(components.as_path());
    std::borrow::Cow::Owned(ordinary)
}

/// Unix canonical paths are already accepted by Git.
#[cfg(not(windows))]
fn git_path(path: &Path) -> std::borrow::Cow<'_, Path> {
    std::borrow::Cow::Borrowed(path)
}

/// Resolve canonical checkout ownership and reject broken linked metadata instead of selecting main.
fn git_identity(source: &Path) -> io::Result<Option<GitIdentity>> {
    let source = fs::canonicalize(source)?;
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(git_path(&source).as_ref())
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ])
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        _ if !source.ancestors().any(|path| path.join(".git").is_file()) => return Ok(None),
        _ => {
            return Err(invalid(
                "could not inspect linked worktree; refusing primary development fallback",
            ));
        }
    };
    let text = String::from_utf8(output.stdout).map_err(|error| invalid(error.to_string()))?;
    let paths = text
        .lines()
        .map(fs::canonicalize)
        .collect::<io::Result<Vec<_>>>()?;
    let [source, git_dir, common] = paths.as_slice() else {
        return Err(invalid("unexpected Git worktree metadata"));
    };
    Ok(Some(GitIdentity {
        source: source.clone(),
        git_dir: git_dir.clone(),
        linked: git_dir != common,
    }))
}

/// Keep primary and registered backend ports reserved, including removed worktrees' records.
fn reserved_ports(root: &Path) -> io::Result<HashSet<u16>> {
    let mut ports = HashSet::from([5523, 5524, 8080]);
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(valid_id) {
            continue;
        }
        let home = entry.path().join("micromanager");
        let state =
            load_record(&home)?.ok_or_else(|| invalid("missing instance ownership record"))?;
        for port in [state.api_port, state.resource_port] {
            if !ports.insert(port) {
                return Err(invalid(
                    "development instances have conflicting backend ports",
                ));
            }
        }
    }
    Ok(ports)
}

/// Only a Git-linked checkout selects a private VM on managed desktop hosts.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub fn resolve(
    source: &Path,
    primary_home: &Path,
    create: bool,
    dashboard_port: u16,
) -> io::Result<Option<Instance>> {
    resolve_inner(source, primary_home, create, dashboard_port, false)
}

/// Prevent a registered source with missing or changed Git metadata from selecting primary services.
fn reject_registered_fallback(
    source: &Path,
    primary_home: &Path,
    selected: Option<&GitIdentity>,
) -> io::Result<()> {
    let root = primary_home
        .parent()
        .ok_or_else(|| invalid("primary managed home has no parent"))?
        .join("dev-instances");
    if !root.is_dir() {
        return Ok(());
    }
    safe_path(&root)?;
    let source = fs::canonicalize(source)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().to_str().is_some_and(valid_id) {
            continue;
        }
        let state = load_record(&entry.path().join("micromanager"))?
            .ok_or_else(|| invalid("missing worktree ownership"))?;
        if source.starts_with(&state.source)
            && selected.is_none_or(|identity| {
                identity.git_dir != state.git_dir || identity.source != state.source
            })
        {
            return Err(invalid(
                "registered worktree Git identity changed or disappeared; refusing primary fallback",
            ));
        }
    }
    Ok(())
}

/// Resolve or create one locked registry entry; Linux may also isolate its primary checkout.
fn resolve_inner(
    source: &Path,
    primary_home: &Path,
    create: bool,
    dashboard_port: u16,
    primary_container: bool,
) -> io::Result<Option<Instance>> {
    let primary_home = absolute(primary_home)?;
    let selected = git_identity(source)?;
    if selected.as_ref().is_none_or(|identity| !identity.linked) {
        reject_registered_fallback(source, &primary_home, selected.as_ref())?;
    }
    let Some(identity) = selected else {
        if primary_container {
            return Err(invalid("container development requires a Git checkout"));
        }
        return Ok(None);
    };
    if !identity.linked && !primary_container {
        return Ok(None);
    }
    let id = format!(
        "{:x}",
        Sha256::digest(identity.git_dir.as_os_str().as_encoded_bytes())
    )[..16]
        .to_owned();
    let root = primary_home
        .parent()
        .ok_or_else(|| invalid("primary managed home has no parent"))?
        .join("dev-instances");
    let directory = root.join(&id);
    let home = directory.join("micromanager");
    if !create && !home.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no development instance for this worktree; run service dev --port PORT first",
        ));
    }
    if !root.exists() {
        private_directory(&root)?;
    }
    safe_path(&root)?;
    let lock_path = root.join("registry.lock");
    safe_path(&lock_path)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;
    let mut reserved = reserved_ports(&root)?;
    if ![0, 5523, 5524, 8080].contains(&dashboard_port) && reserved.contains(&dashboard_port) {
        return Err(invalid(
            "dashboard port is reserved by a development instance's backend",
        ));
    }
    if home.exists() {
        let instance = load(&home)?.ok_or_else(|| invalid("missing private instance metadata"))?;
        if instance.source != identity.source
            || instance.git_dir != identity.git_dir
            || instance.primary_home != primary_home
        {
            return Err(invalid("development instance belongs to another checkout"));
        }
        if [instance.api_port, instance.resource_port].contains(&dashboard_port) {
            return Err(invalid(
                "dashboard port conflicts with this instance's API/resource port",
            ));
        }
        return Ok(Some(instance));
    }
    if directory.exists() {
        return Err(invalid(
            "incomplete development instance; refusing to overwrite it",
        ));
    }
    reserved.insert(dashboard_port);
    let api = super::ports::available(&reserved)?;
    let api_port = api.local_addr()?.port();
    reserved.insert(api_port);
    let resource = super::ports::available(&reserved)?;
    let instance = Instance {
        id,
        source: identity.source,
        git_dir: identity.git_dir,
        primary_home,
        managed_home: home,
        install_dir: directory.join("bin"),
        api_port,
        resource_port: resource.local_addr()?.port(),
    };
    private_directory(&directory)?;
    instance.save()?;
    Ok(Some(instance))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
/// Shared desktop command inputs needed to select a private backend before native dispatch.
struct Options<'a> {
    /// Existing-instance control, or a source build when absent.
    control: Option<super::super::DevCommand>,
    /// Explicit source root for builds, otherwise the process working directory.
    source: Option<&'a Path>,
    /// Requested dashboard port, separate from reserved backend ports.
    port: u16,
    /// Build the uploaded source with the guest release profile.
    release: bool,
    /// Restore the selected environment's installed release instead of uploading source.
    restore: bool,
    /// Permit prerequisite provisioning without an interactive prompt.
    yes: bool,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
/// Extract development-only inputs without intercepting ordinary primary service commands.
fn command_options(command: &super::super::Command) -> Option<Options<'_>> {
    match command {
        super::super::Command::Dev {
            command,
            source,
            port,
            release,
            restore,
            yes,
        } => Some(Options {
            control: *command,
            source: source.as_deref(),
            port: *port,
            release: *release,
            restore: *restore,
            yes: *yes,
        }),
        _ => None,
    }
}

/// Reject foreign dashboard state or listeners before changing a private backend.
fn preflight_dashboard(
    home: &Path,
    control: Option<super::super::DevCommand>,
    requested_port: u16,
    restore: bool,
) -> io::Result<()> {
    use super::super::DevCommand;
    if matches!(control, Some(DevCommand::Start | DevCommand::Restart))
        && !super::dashboard::has_session(home).map_err(|error| invalid(error.to_string()))?
    {
        return Err(invalid(
            "development dashboard settings are missing; run service dev --port PORT first",
        ));
    }
    let saved_port = super::dashboard::port(home).map_err(|error| invalid(error.to_string()))?;
    if !restore && !matches!(control, Some(DevCommand::Stop | DevCommand::Clean)) {
        let port = if control.is_none() {
            requested_port
        } else {
            saved_port
        };
        super::dashboard::check_port(home, port).map_err(|error| invalid(error.to_string()))?;
    }
    Ok(())
}

/// Re-execute only the selected command with private paths, never process-global env changes.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn dispatch(command: &super::super::Command) -> io::Result<Option<i32>> {
    let Some(Options {
        control,
        source,
        port,
        release,
        restore,
        yes,
    }) = command_options(command)
    else {
        return Ok(None);
    };
    #[cfg(target_os = "windows")]
    if std::env::var_os(CHILD).is_some() {
        return Err(invalid(
            "private native VM child mode is not supported on Windows",
        ));
    }
    #[cfg(target_os = "macos")]
    if let Some(child) = std::env::var_os(CHILD) {
        let home = std::env::var_os("FIRECRAB_MICROMANAGER_HOME")
            .ok_or_else(|| invalid("private development home is missing"))?;
        let instance = load(Path::new(&home))?
            .ok_or_else(|| invalid("private development ownership is missing"))?;
        if child != instance.id.as_str()
            || std::env::var_os("FIRECRAB_INSTALL_DIR").as_deref()
                != Some(instance.install_dir.as_os_str())
        {
            return Err(invalid("private development process identity mismatch"));
        }
        let cwd = std::env::current_dir()?;
        let selected = git_identity(source.unwrap_or(&cwd))?
            .ok_or_else(|| invalid("private development checkout is missing"))?;
        if selected.source != instance.source || selected.git_dir != instance.git_dir {
            return Err(invalid(
                "private development process belongs to another checkout",
            ));
        }
        preflight_dashboard(&instance.managed_home, control, port, restore)?;
        return Ok(None);
    }
    #[cfg(target_os = "macos")]
    let primary_home = super::super::macos::lifecycle::Layout::from_process_env()
        .map_err(|error| invalid(error.to_string()))?
        .managed_home;
    #[cfg(target_os = "windows")]
    let primary_home = super::super::windows::lifecycle::Layout::from_process_env()
        .map_err(|error| invalid(error.to_string()))?
        .managed_home;
    #[cfg(target_os = "windows")]
    if load(&primary_home)?.is_some() {
        return Err(invalid(
            "use the worktree directory to select its isolated container; native WSL fallback is disabled",
        ));
    }
    let cwd = std::env::current_dir()?;
    let create = control.is_none() && !restore;
    let Some(instance) = resolve(source.unwrap_or(&cwd), &primary_home, create, port)? else {
        return Ok(None);
    };
    #[cfg(target_os = "windows")]
    return run_container(&instance, control, port, release, restore, yes).map(Some);
    #[cfg(target_os = "macos")]
    {
        let _ = (release, yes);
        preflight_dashboard(&instance.managed_home, control, port, restore)?;
        let status = ProcessCommand::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1))
            .env(CHILD, &instance.id)
            .env("FIRECRAB_MICROMANAGER_HOME", &instance.managed_home)
            .env("FIRECRAB_INSTALL_DIR", &instance.install_dir)
            .status()?;
        Ok(Some(status.code().unwrap_or(1)))
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
/// Change only the selected container and its owned host dashboard after preflight checks.
pub fn run_container(
    instance: &Instance,
    command: Option<super::super::DevCommand>,
    requested_port: u16,
    release: bool,
    restore: bool,
    yes: bool,
) -> io::Result<i32> {
    use super::super::DevCommand;
    let home = &instance.managed_home;
    preflight_dashboard(home, command, requested_port, restore)?;
    let frontend = if restore || matches!(command, Some(DevCommand::Stop | DevCommand::Clean)) {
        None
    } else {
        let source = if command.is_none() {
            super::dashboard::prepare(&instance.source)
                .map_err(|error| invalid(error.to_string()))?
        } else {
            super::dashboard::source(home).map_err(|error| invalid(error.to_string()))?
        };
        let port = if command.is_none() {
            requested_port
        } else {
            super::dashboard::port(home).map_err(|error| invalid(error.to_string()))?
        };
        Some((source, port))
    };
    let code = super::container::run(instance, command, release, restore, yes)?;
    if code != 0 {
        return Ok(code);
    }
    if command == Some(DevCommand::Clean) {
        super::dashboard::clean(home).map_err(|error| invalid(error.to_string()))?;
    } else if restore || command == Some(DevCommand::Stop) {
        super::dashboard::stop(home).map_err(|error| invalid(error.to_string()))?;
    } else if let Some((source, port)) = frontend {
        if command == Some(DevCommand::Restart) {
            super::dashboard::stop(home).map_err(|error| invalid(error.to_string()))?;
        }
        super::dashboard::start(&source, home, port).map_err(|error| invalid(error.to_string()))?;
    }
    Ok(0)
}

#[cfg(target_os = "linux")]
/// Route Linux source development to a private container before native systemd handling.
pub fn linux_dev(
    command: Option<super::super::DevCommand>,
    source: Option<&Path>,
    port: u16,
    release: bool,
    restore: bool,
    yes: bool,
) -> io::Result<i32> {
    let home = std::env::var_os("FIRECRAB_MICROMANAGER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share/Firecrab/micromanager")
        });
    if !home.is_absolute() {
        return Err(invalid(
            "a private development base requires HOME or FIRECRAB_MICROMANAGER_HOME",
        ));
    }
    let cwd = std::env::current_dir()?;
    let instance = resolve_inner(
        source.unwrap_or(&cwd),
        &home,
        command.is_none() && !restore,
        port,
        true,
    )?
    .ok_or_else(|| invalid("development checkout was not selected"))?;
    run_container(&instance, command, port, release, restore, yes)
}

#[cfg(test)]
/// Create validated private fixture state without touching the process's actual managed home.
pub fn test_instance(directory: &Path, id: &str, api_port: u16, resource_port: u16) -> Instance {
    let directory = fs::canonicalize(directory).unwrap();
    let source = directory.join(format!("source-{id}"));
    let git_dir = directory.join(format!("git-{id}"));
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&git_dir).unwrap();
    let parent = directory.join("dev-instances").join(id);
    private_directory(&parent).unwrap();
    let instance = Instance {
        id: id.into(),
        source,
        git_dir,
        primary_home: directory.join("micromanager"),
        managed_home: parent.join("micromanager"),
        install_dir: parent.join("bin"),
        api_port,
        resource_port,
    };
    instance.save().unwrap();
    instance
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "instance_control_tests.rs"]
mod control_tests;
