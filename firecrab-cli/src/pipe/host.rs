//! Privileged Pipe file transactions, systemd controls and generation checks.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::service::env::ServiceEnv;
use crate::shell::{CommandRunner, RealCommandRunner};

use super::config::Config;
use super::{INGRESS_UNIT, TUNNEL_UNIT};
use firecrab_helper_protocol::pipe::{ApplyRequest, Snapshot, UnitStatus};

/// Host IO, engine validation and systemd transaction failures.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
/// Standalone Nginx unit template, with executable and config path placeholders.
const INGRESS_TEMPLATE: &str = include_str!("../../../packaging/systemd/firecrab-ingress.service");
/// Connector unit template that loads its secret through systemd credentials.
const TUNNEL_TEMPLATE: &str = include_str!("../../../packaging/systemd/firecrab-tunnel.service");

/// Reject mutations unless the caller is root on a running systemd host.
pub(super) fn require_root() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        return Err("pipe apply/disable requires root; run this command with sudo".into());
    }
    if !Path::new("/run/systemd/system").is_dir() {
        return Err("pipe apply/disable requires a running Linux systemd host".into());
    }
    Ok(())
}

/// Hold an exclusive nonblocking lock for the entire apply or disable transaction.
pub(super) fn lock() -> Result<fs::File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open("/run/firecrab-pipe-apply.lock")?;
    file.try_lock_exclusive()
        .map_err(|_| "another pipe apply/disable is running")?;
    Ok(file)
}

/// Accept absolute paths that can be inserted into unit and engine configs literally.
fn unit_path(path: &Path) -> Result<&str> {
    let text = path.to_str().ok_or("pipe paths must be UTF-8")?;
    if !path.is_absolute()
        || !text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.'))
    {
        return Err(
            "pipe paths must be absolute and contain only letters, digits, /, -, _ and .".into(),
        );
    }
    Ok(text)
}

/// Resolve an engine path and require a regular executable with safe config syntax.
fn executable(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    unit_path(&path)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not an executable file", path.display()).into());
    }
    Ok(path)
}

/// Run a fixed engine or systemd command and preserve failure stderr for recovery.
fn command(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<()> {
    let output = runner.run(program, args)?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// Read a boolean systemd state such as active or enabled before applying changes.
fn unit_state(runner: &dyn CommandRunner, verb: &str, unit: &str) -> bool {
    runner
        .run("systemctl", &[verb, "--quiet", unit])
        .is_ok_and(|output| output.status.success())
}

/// Renaming a synced file prevents readers from seeing partial JSON or secrets.
fn write_atomic(path: &Path, data: Option<&[u8]>, mode: u32) -> Result<()> {
    let Some(data) = data else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    };
    let directory = path.parent().ok_or("file has no parent directory")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// Previous and desired bytes needed to commit or roll back one managed file.
struct FileChange {
    /// Destination within the managed configuration or unit directory.
    path: PathBuf,
    /// Original contents, or none when rollback must remove a newly created file.
    previous: Option<Vec<u8>>,
    /// Replacement contents, or none when this apply removes the file.
    next: Option<Vec<u8>>,
    /// Required permissions for both replacement and restored contents.
    mode: u32,
}

impl FileChange {
    /// Capture the previous contents and reject symlinks or non-regular destinations.
    fn new(path: PathBuf, next: Option<Vec<u8>>, mode: u32) -> Result<Self> {
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && !metadata.file_type().is_file()
        {
            return Err(format!(
                "refusing to replace a non-regular pipe file: {}",
                path.display()
            )
            .into());
        }
        let previous = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            path,
            previous,
            next,
            mode,
        })
    }

    /// Compare bytes so unchanged credentials do not restart the connector.
    fn changed(&self) -> bool {
        self.previous != self.next
    }
}

/// Read validated tunnel credentials and encode them for the protected managed file.
fn credentials(config: &Config) -> Result<Option<Vec<u8>>> {
    config
        .credentials()?
        .map(|value| serde_json::to_vec_pretty(&value))
        .transpose()
        .map_err(Into::into)
}

/// Receive a bounded desktop-to-guest manifest/credential bundle through stdin.
pub(super) fn apply_bundle(nginx: Option<&Path>, cloudflared: Option<&Path>) -> Result<()> {
    use std::io::Read;
    require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(super::guest::MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > super::guest::MAX_BUNDLE_BYTES {
        return Err("pipe configuration/credentials exceed 1 MiB".into());
    }
    let mut bundle: super::guest::Bundle = serde_json::from_slice(&bytes)?;
    let directory = tempfile::tempdir()?;
    match (&mut bundle.config.tunnel, bundle.credentials) {
        (Some(tunnel), Some(credentials)) => {
            let file = directory.path().join("credentials.json");
            fs::write(&file, serde_json::to_vec(&credentials)?)?;
            tunnel.credentials_file = file;
        }
        (None, None) => {}
        _ => return Err("tunnel configuration and credentials must be provided together".into()),
    }
    let manifest = directory.path().join("pipe.json");
    fs::write(&manifest, serde_json::to_vec(&bundle.config)?)?;
    apply(&manifest, nginx, cloudflared)
}

/// Lock host mutations and apply a manifest with optional engine path overrides.
pub(super) fn apply(
    config_file: &Path,
    nginx: Option<&Path>,
    cloudflared: Option<&Path>,
) -> Result<()> {
    require_root()?;
    let _lock = lock()?;
    apply_locked(config_file, nginx, cloudflared, || Ok(()))
}

/// Resolve engines and apply while the caller holds the Pipe transaction lock.
/// A failed final verifier rolls back local files and service state.
pub(super) fn apply_locked(
    config_file: &Path,
    nginx: Option<&Path>,
    cloudflared: Option<&Path>,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let config = Config::read(config_file)?;
    let env = ServiceEnv::from_process_env();
    let nginx_path = nginx.map(Path::to_path_buf).unwrap_or_else(|| {
        super::engines::nginx_binary().unwrap_or_else(|| PathBuf::from("/usr/sbin/nginx"))
    });
    let cloudflared_path = cloudflared
        .map(Path::to_path_buf)
        .unwrap_or_else(|| super::engines::managed_binary(&env.libdir, "cloudflared"));
    let nginx = executable(&nginx_path).map_err(|error| {
        format!(
            "Nginx is unavailable at {}: {error}; run pipe install or use --nginx",
            nginx_path.display()
        )
    })?;
    let cloudflared = if config.tunnel.is_some() {
        executable(&cloudflared_path).map_err(|error| {
            format!(
                "cloudflared is unavailable at {}: {error}; run pipe install or use --cloudflared",
                cloudflared_path.display()
            )
        })?
    } else {
        cloudflared_path
    };
    apply_with(
        &RealCommandRunner,
        &env.confdir,
        &env.unitdir,
        &config,
        &nginx,
        &cloudflared,
        verify,
    )?;
    println!(
        "PipeIngress configured at {}; routes are owned by {INGRESS_UNIT}",
        config.listen
    );
    if config.tunnel.is_some() {
        println!(
            "PipeTunnel service started; configure each public DNS name to point at the tunnel"
        );
    }
    Ok(())
}

/// Validate staged engine configs, install files, reload units and verify traffic.
/// Restore previous files, enablement and running state if any step fails.
fn apply_with(
    runner: &dyn CommandRunner,
    confdir: &Path,
    unitdir: &Path,
    config: &Config,
    nginx: &Path,
    cloudflared: &Path,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let confdir_text = unit_path(confdir)?;
    unit_path(unitdir)?;
    let nginx_text = unit_path(nginx)?;
    let cloudflared_text = unit_path(cloudflared)?;
    let ingress_unit = INGRESS_TEMPLATE
        .replace("@NGINX@", nginx_text)
        .replace("@CONFDIR@", confdir_text);
    let tunnel_unit = config.tunnel.as_ref().map(|_| {
        TUNNEL_TEMPLATE
            .replace("@CLOUDFLARED@", cloudflared_text)
            .replace("@CONFDIR@", confdir_text)
    });
    let ingress = config.nginx().into_bytes();
    let mut manifest = config.clone();
    if let Some(tunnel) = &mut manifest.tunnel {
        tunnel.credentials_file = confdir.join("pipe/secrets/tunnel-credentials.json");
    }
    let manifest = serde_json::to_vec_pretty(&manifest)?;
    let tunnel = config
        .tunnel_config()
        .map(|value| serde_json::to_vec_pretty(&value))
        .transpose()?;
    let credentials = credentials(config)?;

    // Validate with the engines before changing the last working config.
    let staging = tempfile::tempdir()?;
    let staged_ingress = staging.path().join("ingress.conf");
    fs::write(
        &staged_ingress,
        config.nginx().replace(
            firecrab_helper_protocol::pipe::STATUS_SOCKET,
            unit_path(&staging.path().join("status.sock"))?,
        ),
    )?;
    command(
        runner,
        nginx_text,
        &[
            "-t",
            "-p",
            unit_path(staging.path())?,
            "-c",
            unit_path(&staged_ingress)?,
        ],
    )?;
    if let Some(tunnel) = &tunnel {
        let staged_tunnel = staging.path().join("tunnel.json");
        fs::write(&staged_tunnel, tunnel)?;
        command(
            runner,
            cloudflared_text,
            &[
                "--config",
                unit_path(&staged_tunnel)?,
                "tunnel",
                "ingress",
                "validate",
            ],
        )?;
    }

    let directory = confdir.join("pipe");
    fs::create_dir_all(&directory)?;
    fs::create_dir_all(unitdir)?;
    // Nginx reads this non-secret config again after HUP. Secrets stay private.
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))?;
    let secrets = directory.join("secrets");
    fs::create_dir_all(&secrets)?;
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o700))?;
    let files = [
        FileChange::new(directory.join("ingress.conf"), Some(ingress), 0o644)?,
        FileChange::new(
            unitdir.join(INGRESS_UNIT),
            Some(ingress_unit.into_bytes()),
            0o644,
        )?,
        FileChange::new(directory.join("tunnel.json"), tunnel, 0o600)?,
        FileChange::new(secrets.join("tunnel-credentials.json"), credentials, 0o600)?,
        FileChange::new(
            unitdir.join(TUNNEL_UNIT),
            tunnel_unit.map(String::into_bytes),
            0o644,
        )?,
        FileChange::new(directory.join("manifest.json"), Some(manifest), 0o600)?,
    ];
    let [
        _ingress_file,
        ingress_unit_file,
        tunnel_file,
        credential_file,
        tunnel_unit_file,
        manifest_file,
    ] = &files;
    let ingress_active = unit_state(runner, "is-active", INGRESS_UNIT);
    let tunnel_active = unit_state(runner, "is-active", TUNNEL_UNIT);
    let enabled = [INGRESS_UNIT, TUNNEL_UNIT].map(|unit| unit_state(runner, "is-enabled", unit));
    if ingress_active && ingress_unit_file.changed() {
        return Err(
            "disable the pipe before changing its Nginx executable or config directory".into(),
        );
    }
    let tunnel_changed = [tunnel_file, credential_file, tunnel_unit_file]
        .into_iter()
        .any(FileChange::changed);
    let tunnel_touched = std::cell::Cell::new(false);
    let apply = || -> Result<()> {
        if config.tunnel.is_none() && tunnel_unit_file.previous.is_some() {
            // Disable while the unit file still exists, before daemon-reload.
            tunnel_touched.set(true);
            command(runner, "systemctl", &["disable", "--now", TUNNEL_UNIT])?;
        }
        for file in &files {
            write_atomic(&file.path, file.next.as_deref(), file.mode)?;
        }
        command(runner, "systemctl", &["daemon-reload"])?;
        command(runner, "systemctl", &["enable", INGRESS_UNIT])?;
        let verb = if ingress_active { "reload" } else { "start" };
        command(runner, "systemctl", &[verb, INGRESS_UNIT])?;
        wait_generation(&config.generation())?;
        if config.tunnel.is_some() {
            command(runner, "systemctl", &["enable", TUNNEL_UNIT])?;
            if !tunnel_active || tunnel_changed {
                let verb = if tunnel_active { "restart" } else { "start" };
                tunnel_touched.set(true);
                command(runner, "systemctl", &[verb, TUNNEL_UNIT])?;
            }
        }
        verify()
    };
    if let Err(error) = apply() {
        let rollback = || -> Result<()> {
            for file in &files {
                if file.changed() {
                    write_atomic(&file.path, file.previous.as_deref(), file.mode)?;
                }
            }
            command(runner, "systemctl", &["daemon-reload"])?;
            if ingress_active {
                command(runner, "systemctl", &["reload", INGRESS_UNIT])?;
                if let Some(previous) = &manifest_file.previous {
                    let previous: Config = serde_json::from_slice(previous)?;
                    wait_generation(&previous.generation())?;
                }
            } else {
                let _ = runner.run("systemctl", &["stop", INGRESS_UNIT]);
            }
            if tunnel_touched.get() && tunnel_active {
                command(runner, "systemctl", &["restart", TUNNEL_UNIT])?;
            } else if tunnel_touched.get() && !tunnel_active {
                let _ = runner.run("systemctl", &["stop", TUNNEL_UNIT]);
            }
            for ((unit, file), was_enabled) in [
                (INGRESS_UNIT, ingress_unit_file),
                (TUNNEL_UNIT, tunnel_unit_file),
            ]
            .into_iter()
            .zip(enabled)
            {
                if file.previous.is_some() {
                    command(
                        runner,
                        "systemctl",
                        &[if was_enabled { "enable" } else { "disable" }, unit],
                    )?;
                } else {
                    let _ = runner.run("systemctl", &["disable", unit]);
                }
            }
            Ok(())
        };
        return match rollback() {
            Ok(()) => Err(format!("{error}; previous pipe configuration restored").into()),
            Err(restore_error) => {
                Err(format!("{error}; rollback also failed: {restore_error}").into())
            }
        };
    }
    Ok(())
}

/// Inspect engine availability and live units, optionally including the saved manifest.
pub(super) fn snapshot(include_config: bool) -> Result<Snapshot> {
    let env = ServiceEnv::from_process_env();
    let path = env.confdir.join("pipe/manifest.json");
    let config = if include_config && path.exists() {
        Some(Config::read(&path)?)
    } else {
        None
    };
    let mut units = Vec::new();
    for unit in [INGRESS_UNIT, TUNNEL_UNIT] {
        let output = RealCommandRunner.run(
            "systemctl",
            &[
                "show",
                "--property=LoadState,ActiveState,SubState,MainPID",
                unit,
            ],
        )?;
        let text = String::from_utf8_lossy(&output.stdout);
        let value = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .unwrap_or("unknown")
                .to_owned()
        };
        units.push(UnitStatus {
            unit: unit.into(),
            load_state: value("LoadState"),
            active_state: value("ActiveState"),
            sub_state: value("SubState"),
            main_pid: value("MainPID").parse().unwrap_or(0),
        });
    }
    Ok(Snapshot {
        config,
        units,
        nginx_available: super::engines::nginx_binary().is_some(),
        cloudflared_available: super::engines::managed_binary(&env.libdir, "cloudflared").is_file(),
    })
}

/// Print live unit state as JSON without reading the saved manifest.
pub(super) fn status() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&snapshot(false)?)?);
    Ok(())
}

/// Print a complete dashboard snapshot as JSON for the privileged helper.
pub(super) fn inspect() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&snapshot(true)?)?);
    Ok(())
}

/// Apply a bounded helper request, retaining existing credentials when omitted.
pub(super) fn apply_managed() -> Result<()> {
    use std::io::Read;
    require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(48 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 48 * 1024 {
        return Err("pipe request exceeds 48 KiB".into());
    }
    let request: ApplyRequest = serde_json::from_slice(&bytes)?;
    let mut config = request.config;
    let credentials = if let Some(tunnel) = &mut config.tunnel {
        tunnel.credentials_file = ServiceEnv::from_process_env()
            .confdir
            .join("pipe/secrets/tunnel-credentials.json");
        match request.credentials {
            Some(value) => Some(value),
            None => config.credentials()?,
        }
    } else {
        None
    };
    let directory = tempfile::tempdir()?;
    if let (Some(tunnel), Some(value)) = (&mut config.tunnel, credentials) {
        tunnel.credentials_file = directory.path().join("credentials.json");
        fs::write(&tunnel.credentials_file, serde_json::to_vec(&value)?)?;
    }
    let path = directory.path().join("pipe.json");
    fs::write(&path, serde_json::to_vec(&config)?)?;
    apply(&path, None, None)
}

/// Stop and disable installed Pipe units under the transaction lock.
pub(super) fn disable() -> Result<()> {
    require_root()?;
    let _lock = lock()?;
    for unit in [TUNNEL_UNIT, INGRESS_UNIT] {
        let load = RealCommandRunner.run(
            "systemctl",
            &["show", "--property=LoadState", "--value", unit],
        )?;
        if String::from_utf8_lossy(&load.stdout).trim() == "not-found" {
            continue;
        }
        command(&RealCommandRunner, "systemctl", &["disable", "--now", unit])?;
    }
    Ok(())
}

/// Wait up to eight seconds for Nginx to serve the requested route-generation hash.
pub(crate) fn wait_generation(expected: &str) -> io::Result<()> {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Ok(mut socket) = UnixStream::connect(firecrab_helper_protocol::pipe::STATUS_SOCKET) {
            socket.set_read_timeout(Some(Duration::from_millis(500)))?;
            socket.set_write_timeout(Some(Duration::from_millis(500)))?;
            if socket
                .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .is_ok()
            {
                let mut response = String::new();
                if socket.take(4096).read_to_string(&mut response).is_ok()
                    && response.split("\r\n\r\n").nth(1) == Some(expected)
                {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(io::Error::other(
        "Nginx did not accept the candidate configuration",
    ))
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
