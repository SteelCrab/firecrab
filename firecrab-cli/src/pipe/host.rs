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
use firecrab_helper_protocol::pipe::{
    ApplyRequest, LEGACY_QUICK_TUNNEL_ID, QuickTunnelSnapshot, QuickTunnelTarget, Service,
    ServiceAction, ServiceRequest, Snapshot, UnitStatus, quick_tunnel_host, quick_tunnel_unit,
};

/// Host IO, engine validation and systemd transaction failures.
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
/// Standalone Nginx unit template, with executable and config path placeholders.
const INGRESS_TEMPLATE: &str = include_str!("../../../packaging/systemd/firecrab-ingress.service");
/// Connector unit template that loads its secret through systemd credentials.
const TUNNEL_TEMPLATE: &str = include_str!("../../../packaging/systemd/firecrab-tunnel.service");
/// Anonymous connector, isolated from any host-wide cloudflared configuration.
const QUICK_TUNNEL_TEMPLATE: &str =
    include_str!("../../../packaging/systemd/firecrab-quick-tunnel.service");

/// Reject mutations unless the caller is root on a running systemd host.
pub(super) fn require_root() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        return Err("pipe changes require root; run this command with sudo".into());
    }
    if !Path::new("/run/systemd/system").is_dir() {
        return Err("pipe changes require a running Linux systemd host".into());
    }
    Ok(())
}

/// Serialize configuration changes and service jobs with the same nonblocking lock.
pub(super) fn lock() -> Result<fs::File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open("/run/firecrab-pipe-apply.lock")?;
    file.try_lock_exclusive()
        .map_err(|_| "another pipe change is running")?;
    Ok(file)
}

/// Accept absolute paths that can be inserted into unit and engine configs literally.
pub(super) fn unit_path(path: &Path) -> Result<&str> {
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
pub(super) fn command(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<()> {
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
pub(super) fn write_atomic(path: &Path, data: Option<&[u8]>, mode: u32) -> Result<()> {
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
    apply_mode_locked(
        config_file,
        nginx,
        cloudflared,
        ApplyMode::Configuration,
        verify,
    )
}

/// Apply one anonymous connector intent without resuming other stopped services.
pub(super) fn apply_quick_locked(config_file: &Path) -> Result<()> {
    apply_mode_locked(config_file, None, None, ApplyMode::QuickTunnel, || Ok(()))
}

fn apply_mode_locked(
    config_file: &Path,
    nginx: Option<&Path>,
    cloudflared: Option<&Path>,
    mode: ApplyMode,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let config = Config::read(config_file)?;
    if mode == ApplyMode::Configuration {
        super::firecrab::validate_config(&config)?;
    }
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
    let cloudflared = if config.tunnel.is_some()
        || config.quick_tunnel.is_some()
        || !config.quick_tunnels.is_empty()
    {
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
        (&env.confdir, &env.unitdir),
        &config,
        (&nginx, &cloudflared),
        mode,
        |generation| wait_generation(generation).map_err(Into::into),
        verify,
    )?;
    println!(
        "PipeIngress configured at {}; routes are owned by {INGRESS_UNIT}",
        config.listen
    );
    if config.quick_tunnel.is_some() || !config.quick_tunnels.is_empty() {
        println!("Quick Tunnel configuration saved; inspect Pipe for temporary addresses");
    } else if config.tunnel.is_some() {
        println!(
            "PipeTunnel service started; configure each public DNS name to point at the tunnel"
        );
    }
    Ok(())
}

/// Configuration applies retain the legacy start behavior; Quick intents preserve peers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ApplyMode {
    Configuration,
    QuickTunnel,
}

/// Captured state and file location for one independent connector transaction.
struct TunnelChange {
    unit: String,
    file: usize,
    active: bool,
    enabled: bool,
    changed: bool,
    resume: bool,
    touched: std::cell::Cell<bool>,
}

/// Validate staged files and commit all connector units under one configuration lock.
fn apply_with(
    runner: &dyn CommandRunner,
    directories: (&Path, &Path),
    config: &Config,
    engines: (&Path, &Path),
    mode: ApplyMode,
    check_generation: impl Fn(&str) -> Result<()>,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let (confdir, unitdir) = directories;
    let (nginx, cloudflared) = engines;
    let confdir_text = unit_path(confdir)?;
    unit_path(unitdir)?;
    let nginx_text = unit_path(nginx)?;
    let cloudflared_text = unit_path(cloudflared)?;
    let ingress_unit = INGRESS_TEMPLATE
        .replace("@NGINX@", nginx_text)
        .replace("@CONFDIR@", confdir_text);
    let quick_unit = |host: &str| {
        QUICK_TUNNEL_TEMPLATE
            .replace("@CLOUDFLARED@", cloudflared_text)
            .replace("@CONFDIR@", confdir_text)
            .replace("@LISTEN@", &config.listen.to_string())
            .replace("@QUICK_HOST@", host)
    };
    let legacy_unit = if config.quick_tunnel.is_some() {
        Some(quick_unit(
            firecrab_helper_protocol::pipe::QUICK_TUNNEL_HOST,
        ))
    } else {
        config.tunnel.as_ref().map(|_| {
            TUNNEL_TEMPLATE
                .replace("@CLOUDFLARED@", cloudflared_text)
                .replace("@CONFDIR@", confdir_text)
        })
    };
    let mut manifest = config.clone();
    if let Some(tunnel) = &mut manifest.tunnel {
        tunnel.credentials_file = confdir.join("pipe/secrets/tunnel-credentials.json");
    }
    let manifest = serde_json::to_vec_pretty(&manifest)?;
    let named_config = config
        .tunnel_config()
        .map(|value| serde_json::to_vec_pretty(&value))
        .transpose()?;
    let credentials = credentials(config)?;

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
    if let Some(named_config) = &named_config {
        let staged_tunnel = staging.path().join("tunnel.json");
        fs::write(&staged_tunnel, named_config)?;
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
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))?;
    let secrets = directory.join("secrets");
    fs::create_dir_all(&secrets)?;
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o700))?;
    let uses_quick = config.quick_tunnel.is_some() || !config.quick_tunnels.is_empty();
    let mut files = vec![
        FileChange::new(
            directory.join("ingress.conf"),
            Some(config.nginx().into_bytes()),
            0o644,
        )?,
        FileChange::new(
            unitdir.join(INGRESS_UNIT),
            Some(ingress_unit.into_bytes()),
            0o644,
        )?,
        FileChange::new(directory.join("tunnel.json"), named_config, 0o600)?,
        FileChange::new(
            directory.join("quick-tunnel.json"),
            uses_quick.then(|| b"{}\n".to_vec()),
            0o600,
        )?,
        FileChange::new(secrets.join("tunnel-credentials.json"), credentials, 0o600)?,
        FileChange::new(
            unitdir.join(TUNNEL_UNIT),
            legacy_unit.map(String::into_bytes),
            0o644,
        )?,
        FileChange::new(directory.join("manifest.json"), Some(manifest), 0o600)?,
    ];
    let previous: Option<Config> = files[6]
        .previous
        .as_ref()
        .map(|bytes| serde_json::from_slice(bytes))
        .transpose()?;
    let mut ids: std::collections::BTreeSet<_> =
        config.quick_tunnels.iter().map(|entry| entry.id).collect();
    if let Some(previous) = &previous {
        ids.extend(previous.quick_tunnels.iter().map(|entry| entry.id));
    }
    let mut tunnel_files = vec![(TUNNEL_UNIT.to_owned(), 5, mode == ApplyMode::Configuration)];
    for id in ids {
        let definition = config
            .quick_tunnels
            .iter()
            .find(|entry| entry.id == id)
            .map(|_| quick_unit(&quick_tunnel_host(id)).into_bytes());
        let unit = quick_tunnel_unit(id);
        let index = files.len();
        files.push(FileChange::new(unitdir.join(&unit), definition, 0o644)?);
        tunnel_files.push((unit, index, false));
    }
    let mut tunnels = Vec::new();
    for (unit, file, resume) in tunnel_files {
        if files[file].previous.is_none() && files[file].next.is_none() {
            continue;
        }
        let changed = files[file].changed()
            || (unit == TUNNEL_UNIT && [2, 3, 4].iter().any(|&index| files[index].changed()));
        tunnels.push(TunnelChange {
            active: unit_state(runner, "is-active", &unit),
            enabled: unit_state(runner, "is-enabled", &unit),
            unit,
            file,
            changed,
            resume,
            touched: std::cell::Cell::new(false),
        });
    }
    let ingress_active = unit_state(runner, "is-active", INGRESS_UNIT);
    let ingress_enabled = unit_state(runner, "is-enabled", INGRESS_UNIT);
    let ingress_unit_file = &files[1];
    if ingress_active && ingress_unit_file.changed() {
        return Err(
            "disable the pipe before changing its Nginx executable or config directory".into(),
        );
    }
    let connector_will_start = tunnels.iter().any(|tunnel| {
        files[tunnel.file].next.is_some()
            && ((!tunnel.active && (files[tunnel.file].previous.is_none() || tunnel.resume))
                || (tunnel.active && tunnel.changed))
    });
    let run_ingress = ingress_active
        || ingress_unit_file.previous.is_none()
        || mode == ApplyMode::Configuration
        || connector_will_start;
    let apply = || -> Result<()> {
        for tunnel in &tunnels {
            if files[tunnel.file].next.is_none() && files[tunnel.file].previous.is_some() {
                tunnel.touched.set(true);
                command(runner, "systemctl", &["disable", "--now", &tunnel.unit])?;
            }
        }
        for file in &files {
            write_atomic(&file.path, file.next.as_deref(), file.mode)?;
        }
        command(runner, "systemctl", &["daemon-reload"])?;
        if ingress_unit_file.previous.is_none() || mode == ApplyMode::Configuration {
            command(runner, "systemctl", &["enable", INGRESS_UNIT])?;
        }
        if run_ingress {
            command(
                runner,
                "systemctl",
                &[
                    if ingress_active { "reload" } else { "start" },
                    INGRESS_UNIT,
                ],
            )?;
            check_generation(&config.generation())?;
        }
        for tunnel in &tunnels {
            let file = &files[tunnel.file];
            if file.next.is_none() {
                continue;
            }
            if file.previous.is_none() || tunnel.resume {
                tunnel.touched.set(true);
                command(runner, "systemctl", &["enable", &tunnel.unit])?;
            }
            let start = !tunnel.active && (file.previous.is_none() || tunnel.resume);
            if start || (tunnel.active && tunnel.changed) {
                tunnel.touched.set(true);
                command(
                    runner,
                    "systemctl",
                    &[
                        if tunnel.active { "restart" } else { "start" },
                        &tunnel.unit,
                    ],
                )?;
            }
        }
        verify()
    };
    if let Err(error) = apply() {
        // Continue restoring peers even when one rollback operation fails.
        let mut failures = Vec::new();
        for file in &files {
            if file.changed()
                && let Err(error) = write_atomic(&file.path, file.previous.as_deref(), file.mode)
            {
                failures.push(error.to_string());
            }
        }
        if let Err(error) = command(runner, "systemctl", &["daemon-reload"]) {
            failures.push(error.to_string());
        }
        if ingress_active {
            let restored = command(runner, "systemctl", &["reload", INGRESS_UNIT]).and_then(|()| {
                if let Some(previous) = &previous {
                    check_generation(&previous.generation())
                } else {
                    Ok(())
                }
            });
            if let Err(error) = restored {
                failures.push(error.to_string());
            }
        }
        for tunnel in &tunnels {
            if !tunnel.touched.get() {
                continue;
            }
            if tunnel.active {
                if let Err(error) = command(runner, "systemctl", &["restart", &tunnel.unit]) {
                    failures.push(error.to_string());
                }
            } else {
                let _ = runner.run("systemctl", &["stop", &tunnel.unit]);
            }
            if files[tunnel.file].previous.is_some() {
                if let Err(error) = command(
                    runner,
                    "systemctl",
                    &[
                        if tunnel.enabled { "enable" } else { "disable" },
                        &tunnel.unit,
                    ],
                ) {
                    failures.push(error.to_string());
                }
            } else {
                let _ = runner.run("systemctl", &["disable", &tunnel.unit]);
            }
        }
        if !ingress_active {
            let _ = runner.run("systemctl", &["stop", INGRESS_UNIT]);
        }
        if ingress_unit_file.previous.is_some() {
            if let Err(error) = command(
                runner,
                "systemctl",
                &[
                    if ingress_enabled { "enable" } else { "disable" },
                    INGRESS_UNIT,
                ],
            ) {
                failures.push(error.to_string());
            }
        } else {
            let _ = runner.run("systemctl", &["disable", INGRESS_UNIT]);
        }
        return if failures.is_empty() {
            Err(format!("{error}; previous pipe configuration restored").into())
        } else {
            Err(format!("{error}; rollback also failed: {}", failures.join("; ")).into())
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
    snapshot_with(
        &RealCommandRunner,
        config,
        super::engines::nginx_binary().is_some(),
        super::engines::managed_binary(&env.libdir, "cloudflared").is_file(),
    )
}

fn unit_snapshot(runner: &dyn CommandRunner, unit: &str) -> Result<(UnitStatus, String)> {
    let output = runner.run(
        "systemctl",
        &[
            "show",
            "--property=LoadState,ActiveState,SubState,MainPID,InvocationID",
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
    Ok((
        UnitStatus {
            unit: unit.into(),
            load_state: value("LoadState"),
            active_state: value("ActiveState"),
            sub_state: value("SubState"),
            main_pid: value("MainPID").parse().unwrap_or(0),
        },
        value("InvocationID"),
    ))
}

fn snapshot_with(
    runner: &dyn CommandRunner,
    config: Option<Config>,
    nginx_available: bool,
    cloudflared_available: bool,
) -> Result<Snapshot> {
    let (ingress, _) = unit_snapshot(runner, INGRESS_UNIT)?;
    let (legacy, invocation) = unit_snapshot(runner, TUNNEL_UNIT)?;
    let mut quick_tunnels = Vec::new();
    let quick_tunnel = config
        .as_ref()
        .and_then(|config| config.quick_tunnel.as_ref())
        .map(|quick| {
            let public_url = super::quick::public_url(runner, &legacy, &invocation);
            let error = if public_url.is_none() {
                super::quick::error(runner, &legacy, &invocation)
            } else {
                None
            };
            quick_tunnels.push(QuickTunnelSnapshot {
                id: LEGACY_QUICK_TUNNEL_ID,
                name: "Quick Tunnel".into(),
                target: QuickTunnelTarget::Apps {
                    upstreams: quick.upstreams.clone(),
                },
                unit: legacy.clone(),
                public_url: public_url.clone(),
                error,
            });
            firecrab_helper_protocol::pipe::QuickTunnelStatus { public_url }
        });
    if let Some(config) = &config {
        for entry in &config.quick_tunnels {
            let (unit, invocation) = unit_snapshot(runner, &quick_tunnel_unit(entry.id))?;
            let public_url = super::quick::public_url(runner, &unit, &invocation);
            let error = if public_url.is_none() {
                super::quick::error(runner, &unit, &invocation)
            } else {
                None
            };
            quick_tunnels.push(QuickTunnelSnapshot {
                id: entry.id,
                name: entry.name.clone(),
                target: entry.target.clone(),
                unit,
                public_url,
                error,
            });
        }
    }
    Ok(Snapshot {
        firecrab_tunnels: super::firecrab::addresses().unwrap_or_default(),
        config,
        units: vec![ingress, legacy],
        quick_tunnel,
        quick_tunnels,
        nginx_available,
        cloudflared_available,
    })
}

/// Show all known services without contacting the API; corrupt manifests retain basic status.
pub(super) fn status() -> Result<()> {
    let snapshot = snapshot(true).or_else(|_| snapshot(false))?;
    print!("{}", super::status::format(&snapshot));
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

/// Queue one fixed service job without changing configuration or boot enablement.
/// The caller polls unit state because Nginx can drain requests for several minutes.
pub(super) fn control() -> Result<()> {
    use std::io::Read;
    require_root()?;
    let mut bytes = Vec::new();
    std::io::stdin().take(1025).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 {
        return Err("pipe service request exceeds 1 KiB".into());
    }
    let request: ServiceRequest = serde_json::from_slice(&bytes)?;
    let _lock = lock()?;
    control_with(
        &RealCommandRunner,
        &ServiceEnv::from_process_env().confdir,
        &request,
    )
}

/// Validate the installed target before enqueuing exactly one systemd operation.
fn control_with(
    runner: &dyn CommandRunner,
    confdir: &Path,
    request: &ServiceRequest,
) -> Result<()> {
    let (unit, name) = match request.service {
        Service::Ingress => (INGRESS_UNIT, "Nginx"),
        Service::Tunnel => (TUNNEL_UNIT, "Cloudflare Tunnel"),
    };
    if request.action != ServiceAction::Stop {
        let directory = confdir.join("pipe");
        let manifest = directory.join("manifest.json");
        if !manifest.is_file() {
            return Err(format!("{name} is not configured; save Pipe settings first").into());
        }
        let config = Config::read(&manifest)?;
        let files = match request.service {
            Service::Ingress => vec![directory.join("ingress.conf")],
            Service::Tunnel => {
                if config.quick_tunnel.is_some() {
                    vec![
                        directory.join("quick-tunnel.json"),
                        directory.join("ingress.conf"),
                    ]
                } else {
                    let tunnel = config
                        .tunnel
                        .as_ref()
                        .ok_or("Cloudflare Tunnel is not configured; connect it first")?;
                    vec![
                        directory.join("tunnel.json"),
                        tunnel.credentials_file.clone(),
                    ]
                }
            }
        };
        for path in files {
            if !path.is_file() {
                return Err(format!(
                    "{name} configuration is incomplete; save Pipe settings again"
                )
                .into());
            }
        }
    }
    queue_unit(runner, unit, name, request.action)
}

fn queue_unit(
    runner: &dyn CommandRunner,
    unit: &str,
    name: &str,
    action: ServiceAction,
) -> Result<()> {
    let load = runner.run(
        "systemctl",
        &["show", "--property=LoadState", "--value", unit],
    )?;
    let state = String::from_utf8_lossy(&load.stdout);
    if state.trim() == "not-found" && action == ServiceAction::Stop {
        return Ok(());
    }
    if !load.status.success() || state.trim() != "loaded" {
        return Err(format!("{name} unit is not loaded; save Pipe settings first").into());
    }
    let verb = match action {
        ServiceAction::Start => "start",
        ServiceAction::Stop => "stop",
        ServiceAction::Restart => "restart",
    };
    command(runner, "systemctl", &["--no-block", verb, unit])
}

/// The caller holds the Pipe lock and has loaded the current manifest.
pub(super) fn control_quick_locked(
    config: &Config,
    id: uuid::Uuid,
    action: ServiceAction,
) -> Result<()> {
    control_quick_with(
        &RealCommandRunner,
        &ServiceEnv::from_process_env().confdir,
        config,
        id,
        action,
    )
}

fn control_quick_with(
    runner: &dyn CommandRunner,
    confdir: &Path,
    config: &Config,
    id: uuid::Uuid,
    action: ServiceAction,
) -> Result<()> {
    if id == LEGACY_QUICK_TUNNEL_ID && config.quick_tunnel.is_some() {
        return control_with(
            runner,
            confdir,
            &ServiceRequest {
                service: Service::Tunnel,
                action,
            },
        );
    }
    let entry = config
        .quick_tunnels
        .iter()
        .find(|entry| entry.id == id)
        .ok_or("Quick Tunnel was not found")?;
    if action != ServiceAction::Stop
        && ["quick-tunnel.json", "ingress.conf"]
            .iter()
            .any(|file| !confdir.join("pipe").join(file).is_file())
    {
        return Err("Quick Tunnel configuration is incomplete; save it again".into());
    }
    queue_unit(runner, &quick_tunnel_unit(id), &entry.name, action)
}

/// Stop and disable installed Pipe units under the transaction lock.
pub(super) fn disable() -> Result<()> {
    require_root()?;
    let _lock = lock()?;
    let env = ServiceEnv::from_process_env();
    disable_with(&RealCommandRunner, &env.confdir, &env.unitdir)
}

// Stop remains usable when a saved manifest has broken routes or invalid JSON.
fn disable_with(runner: &dyn CommandRunner, confdir: &Path, unitdir: &Path) -> Result<()> {
    let manifest = fs::read(confdir.join("pipe/manifest.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Config>(&bytes).ok());
    let mut units: std::collections::BTreeSet<String> = manifest
        .into_iter()
        .flat_map(|config| config.quick_tunnels)
        .filter(|entry| !entry.id.is_nil())
        .map(|entry| quick_tunnel_unit(entry.id))
        .collect();
    if let Ok(entries) = fs::read_dir(unitdir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let id = name
                .strip_prefix("firecrab-quick-tunnel-")
                .and_then(|value| value.strip_suffix(".service"))
                .and_then(|value| uuid::Uuid::parse_str(value).ok());
            if let Some(id) = id
                && !id.is_nil()
                && name == quick_tunnel_unit(id)
            {
                units.insert(name.to_owned());
            }
        }
    }
    let mut units = units.into_iter().collect::<Vec<_>>();
    units.extend([TUNNEL_UNIT.into(), INGRESS_UNIT.into()]);
    let mut failures = Vec::new();
    for unit in &units {
        let load = runner.run(
            "systemctl",
            &["show", "--property=LoadState", "--value", unit],
        );
        if load.is_ok_and(|load| String::from_utf8_lossy(&load.stdout).trim() == "not-found") {
            continue;
        }
        if let Err(error) = command(runner, "systemctl", &["disable", "--now", unit]) {
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("; ").into());
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
