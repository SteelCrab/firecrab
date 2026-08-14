//! Turns an OCI process config into a service under the injected init.
//!
//! The image's Entrypoint and Cmd are an application, not an operating system.
//! Writing them to `/sbin/init` would steal PID 1 from DHCP and the readiness
//! sentinel. The guest boot script already runs every executable in
//! `/etc/firecrab/services.d`, so this stage drops one script there.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use super::*;

const GUEST_SERVICE: &str = "etc/firecrab/services.d/app";
const GUEST_TOOLBOX: &str = super::provision::GUEST_TOOLBOX;

#[derive(Debug, Deserialize)]
struct RawImageProcess {
    #[serde(default)]
    config: Option<RawContainerConfig>,
}

#[derive(Debug, Deserialize)]
struct RawContainerConfig {
    #[serde(default, rename = "Entrypoint")]
    entrypoint: Option<Vec<String>>,
    #[serde(default, rename = "Cmd")]
    cmd: Option<Vec<String>>,
    #[serde(default, rename = "Env")]
    env: Option<Vec<String>>,
    #[serde(default, rename = "WorkingDir")]
    working_dir: Option<String>,
}

pub(super) fn process_config_from_image_config(
    bytes: &[u8],
) -> Result<OciProcessConfig, ResolveError> {
    let raw: RawImageProcess = serde_json::from_slice(bytes)
        .map_err(|error| ResolveError::MalformedConfig(error.to_string()))?;
    let config = raw.config.unwrap_or(RawContainerConfig {
        entrypoint: None,
        cmd: None,
        env: None,
        working_dir: None,
    });
    let env = config.env.unwrap_or_default();
    for entry in &env {
        if !is_valid_env(entry) {
            return Err(ResolveError::ServiceEnvInvalid {
                entry: entry.clone(),
            });
        }
    }
    Ok(OciProcessConfig {
        entrypoint: config.entrypoint.unwrap_or_default(),
        cmd: config.cmd.unwrap_or_default(),
        env,
        working_dir: config.working_dir.unwrap_or_default(),
    })
}

pub(super) fn install_oci_service(
    rootfs: &ProvisionedRootfs,
    process: &OciProcessConfig,
) -> Result<(), ResolveError> {
    let argv = process.argv();
    if argv.is_empty() {
        return Ok(());
    }
    let path = rootfs.path().join(GUEST_SERVICE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| service_io("create services.d", parent, source))?;
    }
    let script = render_service(process, &argv);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o755)
        .open(&path)
        .map_err(|source| service_io("create service", &path, source))?;
    file.write_all(script.as_bytes())
        .map_err(|source| service_io("write service", &path, source))?;
    file.set_permissions(std::fs::Permissions::from_mode(0o755))
        .map_err(|source| service_io("chmod service", &path, source))?;
    Ok(())
}

fn render_service(process: &OciProcessConfig, argv: &[&str]) -> String {
    let mut script = format!(
        "#!{GUEST_TOOLBOX} sh\n\
         # Firecrab service for the imported image entrypoint (public-docs/oci.md).\n\
         # This is not PID 1. The injected init keeps DHCP and the readiness sentinel.\n"
    );
    if !process.working_dir.is_empty() {
        script.push_str(&format!("cd {}\n", posix_quote(&process.working_dir)));
    }
    for entry in &process.env {
        let (key, value) = entry.split_once('=').expect("validated Env");
        script.push_str(&format!("export {key}={}\n", posix_quote(value)));
    }
    script.push_str("exec");
    for arg in argv {
        script.push(' ');
        script.push_str(&posix_quote(arg));
    }
    script.push('\n');
    script
}

fn is_valid_env(entry: &str) -> bool {
    match entry.split_once('=') {
        Some((key, _)) => {
            let mut chars = key.chars();
            let Some(first) = chars.next() else {
                return false;
            };
            (first.is_ascii_alphabetic() || first == '_')
                && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        }
        None => false,
    }
}

fn posix_quote(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn service_io(operation: &'static str, path: &Path, source: io::Error) -> ResolveError {
    ResolveError::ServiceIo {
        operation,
        path: path.to_owned(),
        source,
    }
}
