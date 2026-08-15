//! Turns an OCI process config into a service under the injected init.
//!
//! The image's Entrypoint and Cmd are an application, not an operating system.
//! Writing them to `/sbin/init` would steal PID 1 from DHCP and the readiness
//! sentinel. The guest boot script already runs every executable in
//! `/etc/firecrab/services.d`, so this stage drops one script there.

use std::collections::BTreeMap;
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

const VM_ENV_BEGIN: &str = "# >>> firecrab vm env";
const VM_ENV_END: &str = "# <<< firecrab vm env";

/// Single-quote a value for `export KEY=...` so `$`, quotes, and newlines stay literal.
pub(crate) fn posix_quote(value: &str) -> String {
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

/// Rewrites the delimited Firecrab env block in `services.d/app`.
///
/// Image `export` lines stay. A present block is replaced wholesale.
/// Absent + non-empty env inserts after the last image `export ` line and
/// before `exec`. An empty map deletes the block and markers. `BTreeMap`
/// key order plus [`posix_quote`] make repeated applies byte-identical.
pub(crate) fn rewrite_vm_env_block(script: &str, env: &BTreeMap<String, String>) -> String {
    let without = strip_vm_env_block(script);
    if env.is_empty() {
        return without;
    }
    insert_vm_env_block(&without, &render_vm_env_block(env))
}

fn render_vm_env_block(env: &BTreeMap<String, String>) -> String {
    let mut block = String::from(VM_ENV_BEGIN);
    block.push('\n');
    for (key, value) in env {
        block.push_str("export ");
        block.push_str(key);
        block.push('=');
        block.push_str(&posix_quote(value));
        block.push('\n');
    }
    block.push_str(VM_ENV_END);
    block.push('\n');
    block
}

fn strip_vm_env_block(script: &str) -> String {
    let Some(begin) = find_line_index(script, VM_ENV_BEGIN) else {
        return script.to_owned();
    };
    let after_begin = begin + VM_ENV_BEGIN.len();
    let Some(end_rel) = find_unquoted_line_index(&script[after_begin..], VM_ENV_END) else {
        return script.to_owned();
    };
    let end = after_begin + end_rel;
    let mut after_end = end + VM_ENV_END.len();
    if script[after_end..].starts_with('\n') {
        after_end += 1;
    }
    let mut out = String::with_capacity(script.len());
    out.push_str(&script[..begin]);
    out.push_str(&script[after_end..]);
    out
}

fn insert_vm_env_block(script: &str, block: &str) -> String {
    let insert_at = vm_env_insertion_index(script);
    let mut out = String::with_capacity(script.len() + block.len());
    out.push_str(&script[..insert_at]);
    out.push_str(block);
    out.push_str(&script[insert_at..]);
    out
}

fn vm_env_insertion_index(script: &str) -> usize {
    let mut last_export_end = None;
    let mut exec_start = None;
    let mut pos = 0;
    for line in script.split_inclusive('\n') {
        let content = line.trim_end_matches('\n');
        if content.starts_with("export ") {
            last_export_end = Some(pos + line.len());
        }
        if exec_start.is_none() && (content == "exec" || content.starts_with("exec ")) {
            exec_start = Some(pos);
        }
        pos += line.len();
    }
    if let Some(after_export) = last_export_end {
        if exec_start.is_none_or(|exec| after_export <= exec) {
            return after_export;
        }
    }
    exec_start.unwrap_or(script.len())
}

fn find_line_index(script: &str, marker: &str) -> Option<usize> {
    if line_at(script, 0, marker) {
        return Some(0);
    }
    let mut offset = 0;
    while let Some(rel) = script[offset..].find('\n') {
        let start = offset + rel + 1;
        if line_at(script, start, marker) {
            return Some(start);
        }
        offset = start;
    }
    None
}

/// Like [`find_line_index`], but ignores lines that sit inside a POSIX
/// single-quoted value (`export KEY='...`, including `'\''` and newlines).
fn find_unquoted_line_index(script: &str, marker: &str) -> Option<usize> {
    let mut in_single = false;
    let mut i = 0;
    while i < script.len() {
        if !in_single && (i == 0 || script.as_bytes()[i - 1] == b'\n') && line_at(script, i, marker)
        {
            return Some(i);
        }
        let rest = &script[i..];
        if !in_single && rest.starts_with("\\'") {
            i += 2;
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if ch == '\'' {
            in_single = !in_single;
        }
        i += ch.len_utf8();
    }
    None
}

fn line_at(script: &str, start: usize, marker: &str) -> bool {
    let rest = &script[start..];
    rest.starts_with(marker)
        && (rest.len() == marker.len() || rest.as_bytes().get(marker.len()) == Some(&b'\n'))
}

fn service_io(operation: &'static str, path: &Path, source: io::Error) -> ResolveError {
    ResolveError::ServiceIo {
        operation,
        path: path.to_owned(),
        source,
    }
}
