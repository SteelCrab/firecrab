//! Windows and Linux worktrees use a local Docker engine, never host services.

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde_json::Value;

use super::instance::Instance;
use super::{Checkout, guest_script, session};
use crate::micromanager::{DevCommand, report};

/// Debian/systemd image recipe copied into this instance's temporary build context.
const DOCKERFILE: &str = include_str!("../../../../scripts/micromanager/dev-container.Dockerfile");
/// PID 1 setup that verifies KVM, TUN, and writable private kernel namespaces.
const ENTRYPOINT: &str =
    include_str!("../../../../scripts/micromanager/dev-container-entrypoint.sh");
/// Install the verified release baseline before applying a source deployment.
const BOOTSTRAP: &str = include_str!("../../../../scripts/micromanager/dev-container-bootstrap.sh");
/// Relay the container's published address to the API's internal loopback listener.
const PUBLISH: &str = include_str!("../../../../scripts/micromanager/dev-container-publish.sh");
/// Label binding a Docker object to the persistent worktree instance ID.
const OWNER: &str = "dev.firecrab.instance";
/// Label binding a Docker object to the canonical source checkout.
const SOURCE: &str = "dev.firecrab.source";
/// Label preserving Git-directory identity when the checkout changes branches.
const GIT_DIR: &str = "dev.firecrab.git-dir";

/// Keep Docker diagnostics available through the common I/O error boundary.
fn failed(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
}

/// Match Docker's object-specific 404 messages without hiding transport or permission errors.
fn missing_object(stderr: &str, kind: &str, name: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("no such object")
        || stderr.contains(&format!("no such {kind}"))
        || (kind == "network"
            && stderr.contains(&format!("network {} not found", name.to_ascii_lowercase())))
}

/// Permit only local sockets, named pipes, and loopback TCP Docker endpoints.
fn local_endpoint(endpoint: &str) -> bool {
    if endpoint.chars().any(char::is_control) {
        return false;
    }
    if let Some(path) = endpoint.strip_prefix("unix://") {
        return path.starts_with('/') && !path.starts_with("//") && path.len() > 1;
    }
    if let Some(pipe) = endpoint.strip_prefix("npipe:////./pipe/") {
        return !pipe.is_empty() && !pipe.contains(['/', '\\']);
    }
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    url.scheme() == "tcp"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/")
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host == "[::1]"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

/// Resolve Docker's context-over-host precedence and reject remote engines.
fn endpoint_from(
    host: Option<&str>,
    context: Option<&str>,
    inspect: impl FnOnce(Option<&str>) -> io::Result<String>,
) -> io::Result<String> {
    let endpoint = if let Some(context) = context.filter(|value| !value.is_empty()) {
        inspect(Some(context))?
    } else if let Some(host) = host.filter(|value| !value.is_empty()) {
        host.to_owned()
    } else {
        inspect(None)?
    };
    if !local_endpoint(&endpoint) {
        return Err(failed(
            "worktree development requires a local Docker Unix socket, Windows named pipe, or loopback TCP endpoint; remote Docker contexts are not allowed",
        ));
    }
    Ok(endpoint)
}

/// A CLI client pinned to one inspected local engine for the entire operation.
struct Docker {
    /// Docker executable; tests substitute a recording fixture program.
    program: PathBuf,
    /// Explicit `--host` value, independent of later environment changes.
    endpoint: String,
    #[cfg(test)]
    /// Private directory used only by the fake Docker command.
    fixture: Option<PathBuf>,
}

/// Engine properties needed to build and create a compatible private runtime.
struct Engine {
    /// Docker build/create platform derived from the engine's native architecture.
    platform: &'static str,
    /// Whether the engine advertises AppArmor and accepts its security option.
    apparmor: bool,
}

impl Docker {
    /// Read local client configuration without changing contexts or contacting WSL.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn local() -> io::Result<Self> {
        let host = std::env::var("DOCKER_HOST").ok();
        let context = std::env::var("DOCKER_CONTEXT").ok();
        let endpoint = endpoint_from(host.as_deref(), context.as_deref(), |context| {
            // Context inspection reads client configuration; it never contacts an engine.
            let mut command = Command::new("docker");
            command.args([
                "context",
                "inspect",
                "--format",
                "{{json .Endpoints.docker.Host}}",
            ]);
            if let Some(context) = context {
                command.arg("--").arg(context);
            }
            let output = command.output().map_err(|error| {
                failed(format!(
                    "Docker is required for isolated worktree development: {error}"
                ))
            })?;
            if !output.status.success() {
                return Err(failed(
                    "could not inspect the selected local Docker context",
                ));
            }
            serde_json::from_slice::<String>(&output.stdout)
                .map_err(|error| failed(format!("invalid Docker context endpoint: {error}")))
        })?;
        Ok(Self {
            program: "docker".into(),
            endpoint,
            #[cfg(test)]
            fixture: None,
        })
    }

    /// Pin each child to the selected engine and remove conflicting Docker globals.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .env_remove("DOCKER_HOST")
            .env_remove("DOCKER_CONTEXT")
            .env_remove("DOCKER_DEFAULT_PLATFORM")
            .args(["--host", &self.endpoint]);
        #[cfg(test)]
        if let Some(path) = &self.fixture {
            command.env("FIRECRAB_DOCKER_FIXTURE", path);
        }
        command
    }

    /// Capture a noninteractive metadata query for validation by its caller.
    fn output(&self, args: &[&str]) -> io::Result<Output> {
        self.command().args(args).stdin(Stdio::null()).output()
    }

    /// Run one owned-resource operation and propagate its nonzero exit status.
    fn checked(&self, args: &[&str]) -> io::Result<()> {
        if self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .status()?
            .success()
        {
            return Ok(());
        }
        Err(failed(format!(
            "private Docker {} failed; inspect only this instance's container logs",
            args[0]
        )))
    }

    /// Distinguish an absent named object from failed or malformed inspection.
    fn inspect(&self, kind: &str, name: &str) -> io::Result<Option<Value>> {
        let output = self.output(&[kind, "inspect", "--format", "{{json .}}", name])?;
        if output.status.success() {
            return serde_json::from_slice(&output.stdout)
                .map(Some)
                .map_err(|error| failed(format!("invalid Docker {kind} metadata: {error}")));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if missing_object(&stderr, kind, name) {
            return Ok(None);
        }
        Err(failed(format!(
            "could not inspect private Docker {kind}: {}",
            stderr.trim()
        )))
    }

    /// Require a rootful Linux engine with cgroup v2 and a supported architecture.
    fn engine(&self) -> io::Result<Engine> {
        let output = self.output(&["info", "--format", "{{json .}}"])?;
        if !output.status.success() {
            return Err(failed("the selected local Docker engine is unavailable"));
        }
        let info: Value = serde_json::from_slice(&output.stdout).map_err(failed)?;
        let rootless = info["SecurityOptions"].as_array().is_some_and(|options| {
            options.iter().any(|option| {
                option
                    .as_str()
                    .is_some_and(|value| value.contains("rootless"))
            })
        });
        if info["OSType"] != "linux" || info["CgroupVersion"] != "2" || rootless {
            return Err(failed(
                "private Firecrab containers require a local rootful Linux Docker engine with cgroup v2, /dev/kvm and /dev/net/tun; Windows/WSL2 must provide nested KVM",
            ));
        }
        let platform = match info["Architecture"].as_str() {
            Some("x86_64" | "amd64") => "linux/amd64",
            Some("aarch64" | "arm64") => "linux/arm64",
            _ => {
                return Err(failed(
                    "private Firecrab containers require an ARM64 or x86_64 Linux engine",
                ));
            }
        };
        let apparmor = info["SecurityOptions"].as_array().is_some_and(|options| {
            options.iter().any(|option| {
                option
                    .as_str()
                    .is_some_and(|value| value.contains("apparmor"))
            })
        });
        Ok(Engine { platform, apparmor })
    }
}

/// Produce the three ownership labels shared by the image, container, and storage.
fn labels(instance: &Instance) -> io::Result<[(String, String); 3]> {
    let utf8 = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| failed("Docker worktree paths must be valid UTF-8"))
    };
    Ok([
        (OWNER.into(), instance.id.clone()),
        (SOURCE.into(), utf8(&instance.source)?),
        (GIT_DIR.into(), utf8(&instance.git_dir)?),
    ])
}

/// Refuse an existing Docker object unless all persistent ownership labels match.
fn owned(metadata: &Value, kind: &str, instance: &Instance) -> io::Result<()> {
    let actual = if matches!(kind, "container" | "image") {
        &metadata["Config"]["Labels"]
    } else {
        &metadata["Labels"]
    };
    for (key, value) in labels(instance)? {
        if actual[&key] != value {
            return Err(failed(format!(
                "refusing foreign Docker {kind}: worktree ownership does not match"
            )));
        }
    }
    Ok(())
}

/// Validate local private storage and reject networks containing other containers.
fn storage_owned(
    docker: &Docker,
    instance: &Instance,
    name: &str,
    volume: &str,
    required: bool,
) -> io::Result<()> {
    for (kind, resource) in [("network", name), ("volume", volume)] {
        let Some(metadata) = docker.inspect(kind, resource)? else {
            if required {
                return Err(failed(format!("private Docker {kind} is missing")));
            }
            continue;
        };
        owned(&metadata, kind, instance)?;
        let no_options = metadata["Options"].is_null()
            || metadata["Options"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty);
        let driver = if kind == "network" { "bridge" } else { "local" };
        if metadata["Driver"] != driver || metadata["Scope"] != "local" || !no_options {
            return Err(failed(format!(
                "refusing Docker {kind} with non-private storage or driver options"
            )));
        }
        if kind == "network"
            && metadata["Containers"]
                .as_object()
                .is_some_and(|containers| {
                    containers
                        .values()
                        .any(|container| container["Name"] != name)
                })
        {
            return Err(failed(
                "another container is attached to this worktree's private network",
            ));
        }
    }
    Ok(())
}

/// Verify ownership plus the exact namespace, device, publication, and mount limits.
fn container_owned(
    metadata: &Value,
    instance: &Instance,
    name: &str,
    volume: &str,
) -> io::Result<()> {
    owned(metadata, "container", instance)?;
    let config = &metadata["HostConfig"];
    let bindings = &config["PortBindings"];
    let publication = &bindings["5523/tcp"];
    let safe = config["Privileged"] == false
        && config["NetworkMode"] == name
        && metadata["NetworkSettings"]["Networks"]
            .as_object()
            .is_some_and(|networks| networks.len() == 1 && networks.contains_key(name))
        && matches!(config["PidMode"].as_str(), Some("" | "private"))
        && config["UTSMode"].as_str() == Some("")
        && config["UsernsMode"].as_str() == Some("")
        && config["IpcMode"] == "private"
        && config["CgroupnsMode"] == "private"
        && config["CapAdd"].as_array().is_some_and(|caps| {
            caps.len() == 2
                && ["SYS_ADMIN", "NET_ADMIN"].iter().all(|required| {
                    caps.iter().any(|cap| {
                        cap.as_str()
                            .is_some_and(|cap| cap.strip_prefix("CAP_").unwrap_or(cap) == *required)
                    })
                })
        })
        && config["Devices"].as_array().is_some_and(|devices| {
            devices.len() == 2
                && ["/dev/kvm", "/dev/net/tun"].iter().all(|path| {
                    devices.iter().any(|device| {
                        device["PathOnHost"] == *path
                            && device["PathInContainer"] == *path
                            && device["CgroupPermissions"] == "rwm"
                    })
                })
        })
        && config["DeviceRequests"]
            .as_array()
            .is_none_or(Vec::is_empty)
        && config["DeviceCgroupRules"]
            .as_array()
            .is_none_or(Vec::is_empty)
        && config["Binds"].as_array().is_none_or(Vec::is_empty)
        && config["Tmpfs"]
            .as_object()
            .is_some_and(|mounts| mounts.len() == 3)
        && config["Tmpfs"]["/run"] == ""
        && config["Tmpfs"]["/run/lock"] == ""
        && config["Tmpfs"]["/tmp"] == "exec,nosuid,nodev"
        && bindings.as_object().is_some_and(|ports| ports.len() == 1)
        && publication.as_array().is_some_and(|ports| ports.len() == 1)
        && publication[0]["HostIp"] == "127.0.0.1"
        && publication[0]["HostPort"]
            .as_str()
            .and_then(|value| value.parse::<u16>().ok())
            == Some(instance.api_port)
        && metadata["Mounts"].as_array().is_some_and(|mounts| {
            mounts
                .iter()
                .filter(|mount| mount["Type"] == "volume")
                .count()
                == 1
                && mounts.iter().all(|mount| {
                    (mount["Type"] == "volume"
                        && mount["Name"] == volume
                        && mount["Destination"] == "/var/lib/firecrab")
                        || (mount["Type"] == "tmpfs"
                            && matches!(
                                mount["Destination"].as_str(),
                                Some("/run" | "/run/lock" | "/tmp")
                            ))
                })
        });
    if !safe {
        return Err(failed(
            "refusing a Docker container with foreign mounts, ports, shared namespaces or unsupported tmpfs options",
        ));
    }
    Ok(())
}

/// Append ownership flags without shell interpolation of checkout paths.
fn add_labels(args: &mut Vec<OsString>, instance: &Instance) -> io::Result<()> {
    for (key, value) in labels(instance)? {
        args.extend(["--label".into(), format!("{key}={value}").into()]);
    }
    Ok(())
}

/// Build and create only this instance's labeled image, network, volume, and container.
///
/// Existing storage is validated both before creation and immediately before it is
/// mounted. The runtime receives KVM/TUN and private namespaces, never host paths.
fn create_runtime(
    docker: &Docker,
    instance: &Instance,
    name: &str,
    volume: &str,
    engine: &Engine,
    yes: bool,
) -> io::Result<()> {
    storage_owned(docker, instance, name, volume, false)?;
    if !yes && io::stdin().is_terminal() && io::stdout().is_terminal() {
        print!(
            "Create the private Docker runtime and download Debian, compiler tools and a Firecrab release? [y/N] "
        );
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
            return Err(failed("private Docker runtime creation cancelled"));
        }
    }
    let image = format!("{name}:runtime-v1");
    if let Some(metadata) = docker.inspect("image", &image)? {
        owned(&metadata, "image", instance)?;
    }
    let context = tempfile::tempdir()?;
    fs::create_dir(context.path().join("scripts"))?;
    for (file, text) in [
        ("Dockerfile", DOCKERFILE),
        ("dev-container-entrypoint.sh", ENTRYPOINT),
        ("dev-container-bootstrap.sh", BOOTSTRAP),
        ("dev-container-publish.sh", PUBLISH),
        ("guest-dev.sh", super::GUEST_SCRIPT),
    ] {
        fs::write(context.path().join(file), text.replace("\r\n", "\n"))?;
    }
    for file in [
        "install.sh",
        "scripts/firecrab-release.sh",
        "scripts/install-firecracker.sh",
    ] {
        let text = fs::read_to_string(instance.source.join(file))?;
        fs::write(context.path().join(file), text.replace("\r\n", "\n"))?;
    }
    let mut args: Vec<OsString> = ["build", "--platform", engine.platform, "--tag", &image]
        .into_iter()
        .map(Into::into)
        .collect();
    add_labels(&mut args, instance)?;
    args.push(context.path().into());
    report!("[BUILD] private Docker runtime: {name}");
    if !docker
        .command()
        .args(args)
        .stdin(Stdio::null())
        .status()?
        .success()
    {
        return Err(failed("private Docker image build failed"));
    }
    for (kind, resource) in [("network", name), ("volume", volume)] {
        if let Some(metadata) = docker.inspect(kind, resource)? {
            owned(&metadata, kind, instance)?;
        } else {
            let mut args: Vec<OsString> = [kind, "create"].into_iter().map(Into::into).collect();
            if kind == "network" {
                args.extend(["--driver".into(), "bridge".into()]);
            }
            add_labels(&mut args, instance)?;
            args.push(resource.into());
            if !docker
                .command()
                .args(args)
                .stdin(Stdio::null())
                .status()?
                .success()
            {
                return Err(failed(format!("could not create private Docker {kind}")));
            }
        }
    }
    // A concurrent create must not let a foreign same-name volume become a mount.
    storage_owned(docker, instance, name, volume, true)?;
    let publication = format!("127.0.0.1:{}:5523/tcp", instance.api_port);
    let mount = format!("type=volume,source={volume},target=/var/lib/firecrab");
    let mut args: Vec<OsString> = [
        "create",
        "--name",
        name,
        "--hostname",
        name,
        "--platform",
        engine.platform,
        "--network",
        name,
        "--ipc",
        "private",
        "--cgroupns",
        "private",
        "--cap-add",
        "SYS_ADMIN",
        "--cap-add",
        "NET_ADMIN",
        "--security-opt",
        "seccomp=unconfined",
        "--device",
        "/dev/kvm:/dev/kvm:rwm",
        "--device",
        "/dev/net/tun:/dev/net/tun:rwm",
        "--tmpfs",
        "/run",
        "--tmpfs",
        "/run/lock",
        "--tmpfs",
        "/tmp:exec,nosuid,nodev",
        "--mount",
        &mount,
        "--publish",
        &publication,
        "--stop-timeout",
        "30",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if engine.apparmor {
        args.extend(["--security-opt".into(), "apparmor=unconfined".into()]);
    }
    add_labels(&mut args, instance)?;
    args.push(image.into());
    if !docker
        .command()
        .args(args)
        .stdin(Stdio::null())
        .status()?
        .success()
    {
        return Err(failed(
            "could not create private Firecrab container; the local Linux engine must provide /dev/kvm and /dev/net/tun",
        ));
    }
    Ok(())
}

/// Wait at most sixty seconds for the API on this instance's assigned host port.
fn wait_api(instance: &Instance) -> io::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(failed)?;
    let url = format!("http://127.0.0.1:{}/api/host", instance.api_port);
    let started = Instant::now();
    loop {
        if client
            .get(&url)
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        if started.elapsed() >= Duration::from_secs(60) {
            return Err(failed(
                "private Docker API did not become ready on its assigned localhost port",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

/// Run the shared transactional source/release script inside the selected container.
fn guest(
    docker: &Docker,
    name: &str,
    profile: &str,
    channel: &str,
    archive: &str,
) -> io::Result<()> {
    let mut command = docker.command();
    command.args([
        "exec",
        "-i",
        name,
        "bash",
        "-c",
        &session::runner(),
        "--",
        profile,
        channel,
        archive,
    ]);
    let input = guest_script(true).map_err(|error| failed(error.to_string()))?;
    if session::Input::Session(input)
        .status(&mut command)?
        .success()
    {
        return Ok(());
    }
    Err(failed(
        "private container development transaction failed; inspect that instance's container logs",
    ))
}

/// Serialize lifecycle changes, allowing brief descriptor inheritance during forks.
fn container_lock(home: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(home.join("runtime"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("runtime/container.lock"))?;
    // Another thread's child can briefly inherit this descriptor before exec.
    let started = Instant::now();
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() != fs2::lock_contended_error().kind() => return Err(error),
            Err(_) if started.elapsed() >= Duration::from_secs(1) => {
                return Err(failed(
                    "another development operation owns this worktree container",
                ));
            }
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Validate the selected runtime before source upload or scoped lifecycle changes.
///
/// Stop/clean are harmless when no container exists. Start/restart/restore require
/// an existing owned runtime; none of these controls creates a fallback environment.
fn run_with(
    docker: &Docker,
    instance: &Instance,
    command: Option<DevCommand>,
    release: bool,
    restore: bool,
    yes: bool,
) -> io::Result<i32> {
    if instance.id.len() != 16
        || !instance
            .id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || instance.api_port < 1024
        || [5523, 5524, 8080].contains(&instance.api_port)
        || instance.managed_home == instance.primary_home
    {
        return Err(failed("invalid private Docker development instance"));
    }
    let _lock = container_lock(&instance.managed_home)?;
    let checkout = if command.is_none() && !restore {
        Some(Checkout::prepare(&instance.source).map_err(|error| failed(error.to_string()))?)
    } else {
        None
    };
    let engine = docker.engine()?;
    let name = format!("firecrab-dev-{}", instance.id);
    let volume = format!("{name}-data");
    let previous = docker.inspect("container", &name)?;
    if let Some(metadata) = &previous {
        container_owned(metadata, instance, &name, &volume)?;
        storage_owned(docker, instance, &name, &volume, true)?;
    } else if matches!(command, Some(DevCommand::Stop | DevCommand::Clean)) {
        return Ok(0);
    } else if command.is_some() || restore {
        return Err(failed(
            "this worktree has no Docker runtime; run service dev --port PORT first",
        ));
    }
    if command == Some(DevCommand::Stop) {
        docker.checked(&["stop", "--time", "30", &name])?;
        return Ok(0);
    }
    if previous.is_none() {
        create_runtime(docker, instance, &name, &volume, &engine, yes)?;
        let metadata = docker
            .inspect("container", &name)?
            .ok_or_else(|| failed("created private Docker container is missing"))?;
        container_owned(&metadata, instance, &name, &volume)?;
    }
    if command == Some(DevCommand::Restart) {
        docker.checked(&["restart", "--time", "30", &name])?;
    } else {
        docker.checked(&["start", &name])?;
    }
    docker.checked(&[
        "exec",
        &name,
        "bash",
        "/opt/firecrab-bootstrap/dev-container-bootstrap.sh",
    ])?;
    if let Some(checkout) = checkout {
        let archive = format!("incoming-{}.tar", uuid::Uuid::new_v4());
        let upload = format!(
            "install -d -m 0700 /var/lib/firecrab/dev && umask 077 && cat > /var/lib/firecrab/dev/{archive}"
        );
        let mut command = docker.command();
        command.args(["exec", "-i", &name, "sh", "-c", &upload]);
        let file = checkout.archive.reopen()?;
        if !session::Input::File(file).status(&mut command)?.success() {
            return Err(failed(
                "could not upload this worktree's private source snapshot",
            ));
        }
        guest(
            docker,
            &name,
            if release { "release" } else { "debug" },
            &checkout.channel,
            &archive,
        )?;
    } else {
        let profile = if restore {
            "restore"
        } else {
            command.expect("control command").as_str()
        };
        guest(docker, &name, profile, "unused", "incoming-restore.tar")?;
    }
    wait_api(instance)?;
    report!(
        "[PASS] private Docker API: http://127.0.0.1:{}/",
        instance.api_port
    );
    Ok(0)
}

/// The dashboard is managed separately by the caller using the same private home.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub fn run(
    instance: &Instance,
    command: Option<DevCommand>,
    release: bool,
    restore: bool,
    yes: bool,
) -> io::Result<i32> {
    run_with(&Docker::local()?, instance, command, release, restore, yes)
}

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "container_runtime_tests.rs"]
mod runtime_tests;
