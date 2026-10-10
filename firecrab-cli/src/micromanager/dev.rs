use std::fs;
use std::io;
#[cfg(target_os = "macos")]
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) const GUEST_SCRIPT: &str =
    include_str!("../../../scripts/micromanager/dev-macos-guest.sh");
// Include the workspace manifests and compile-time resources, not the whole
// checkout: local configs, credentials, nested checkouts and build output stay home.
const SOURCE_INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "firecrab-api",
    "firecrab-api-types",
    "firecrab-cli",
    "firecrab-helper-protocol",
    "firecrab-helper",
    "scripts/firecracker-menual",
    "scripts/micromanager/dev-macos-guest.sh",
    "assets/firecrab-motd",
    "packaging/m2images.json",
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid Firecrab checkout {path}: {detail}")]
    Checkout { path: PathBuf, detail: String },
    #[error("could not {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{0} failed; see the command output above")]
    Command(&'static str),
    #[cfg(target_os = "macos")]
    #[error("management VM is not ready; inspect `firecrab service debug --logs`")]
    GuestNotReady,
    #[cfg(target_os = "macos")]
    #[error(
        "management VM SSH at {ip} is not ready: {detail}; inspect `firecrab service debug --logs` before retrying (source upload was not attempted)"
    )]
    GuestSshUnavailable { ip: IpAddr, detail: String },
    #[cfg(target_os = "macos")]
    #[error("management SSH failed during {0}; inspect `firecrab service debug --logs`")]
    GuestSshInterrupted(&'static str),
    #[error(
        "guest services are ready but the localhost API is unavailable; inspect `firecrab service debug --logs`"
    )]
    LocalApiUnavailable,
}

pub struct Checkout {
    pub(super) root: PathBuf,
    pub(super) channel: String,
    pub(super) archive: tempfile::NamedTempFile,
}

impl Checkout {
    pub fn prepare(path: &Path) -> Result<Self, Error> {
        let root =
            fs::canonicalize(path).map_err(|error| checkout_error(path, error.to_string()))?;
        let channel = validate_checkout(&root)?;
        let archive = tempfile::NamedTempFile::new()
            .map_err(|source| io_error("create source archive", source))?;
        let output = archive
            .reopen()
            .map_err(|source| io_error("open source archive output", source))?;
        // Windows tar uses narrow strings for -C/-f paths. Let Rust pass the
        // working directory through CreateProcessW and the archive as a handle
        // so Unicode checkout and TEMP directories do not cross that boundary.
        let status = Command::new(tar_program())
            .args(["--no-xattrs", "-cf", "-"])
            .current_dir(&root)
            .stdout(Stdio::from(output))
            .args([
                "--exclude=.git",
                "--exclude=target",
                "--exclude=node_modules",
                "--exclude=.DS_Store",
                "--exclude=.env",
                "--exclude=.env.*",
            ])
            .args(SOURCE_INPUTS)
            .env("COPYFILE_DISABLE", "1")
            .status()
            .map_err(|source| io_error("archive checkout sources", source))?;
        if !status.success() {
            return Err(Error::Command("source archive"));
        }
        Ok(Self {
            root,
            channel,
            archive,
        })
    }
}

fn validate_checkout(root: &Path) -> Result<String, Error> {
    for input in SOURCE_INPUTS {
        let path = root.join(input);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| checkout_error(root, format!("{input}: {error}")))?;
        if metadata.is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(checkout_error(
                root,
                format!("{input} must be a regular file or directory"),
            ));
        }
    }
    let manifest = read_toml(root, "Cargo.toml")?;
    let members = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| checkout_error(root, "workspace members are missing"))?;
    // Cargo resolves every member's manifest even when only two packages build.
    for required in SOURCE_INPUTS
        .iter()
        .filter(|input| input.starts_with("firecrab-"))
    {
        if !members
            .iter()
            .any(|member| member.as_str() == Some(required))
        {
            return Err(checkout_error(
                root,
                format!("workspace member {required} is missing"),
            ));
        }
    }
    if members.len() != 5 {
        return Err(checkout_error(
            root,
            "development archive supports the five Firecrab workspace members",
        ));
    }
    let toolchain = read_toml(root, "rust-toolchain.toml")?;
    let channel = toolchain
        .get("toolchain")
        .and_then(|toolchain| toolchain.get("channel"))
        .and_then(toml::Value::as_str)
        .filter(|channel| {
            !channel.is_empty()
                && channel
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
        })
        .ok_or_else(|| checkout_error(root, "Rust toolchain channel is missing or invalid"))?;
    Ok(channel.to_string())
}

fn read_toml(root: &Path, name: &str) -> Result<toml::Value, Error> {
    let text = fs::read_to_string(root.join(name))
        .map_err(|error| checkout_error(root, format!("{name}: {error}")))?;
    toml::from_str(&text).map_err(|error| checkout_error(root, format!("{name}: {error}")))
}

fn tar_program() -> &'static str {
    if cfg!(target_os = "windows") {
        "tar.exe"
    } else {
        "/usr/bin/tar"
    }
}

/// LF-only even when the CLI was built from a Windows checkout with CRLF.
pub(super) fn guest_script() -> Result<std::fs::File, Error> {
    use std::io::{Seek, Write};
    let mut script =
        tempfile::tempfile().map_err(|source| io_error("create guest build script", source))?;
    script
        .write_all(GUEST_SCRIPT.replace("\r\n", "\n").as_bytes())
        .and_then(|_| script.rewind())
        .map_err(|source| io_error("write guest build script", source))?;
    Ok(script)
}

fn checkout_error(path: &Path, detail: impl Into<String>) -> Error {
    Error::Checkout {
        path: path.to_owned(),
        detail: detail.into(),
    }
}

pub(super) fn io_error(action: &'static str, source: io::Error) -> Error {
    Error::Io { action, source }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(crate) fn checkout_fixture() -> tempfile::TempDir {
        let directory = tempfile::Builder::new()
            .prefix("firecrab dev 소스 ")
            .tempdir()
            .unwrap();
        for input in SOURCE_INPUTS {
            let path = directory.path().join(input);
            if input.starts_with("firecrab-") || *input == "scripts/firecracker-menual" {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, "").unwrap();
            }
        }
        fs::write(directory.path().join("Cargo.toml"), r#"
[workspace]
members = ["firecrab-api", "firecrab-api-types", "firecrab-cli", "firecrab-helper-protocol", "firecrab-helper"]
"#).unwrap();
        fs::write(
            directory.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = '1.97.1'\n",
        )
        .unwrap();
        directory
    }

    #[test]
    fn source_archive_contains_edited_sources_and_compile_time_resources_only() {
        let directory = checkout_fixture();
        let source = directory.path().join("firecrab-api/src/main.rs");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(source, "edited local source").unwrap();
        for name in [
            "config.toml",
            "firecrab-api/.env",
            "firecrab-api/target/old-binary",
            "firecrab-api/.git/config",
        ] {
            let path = directory.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "must stay on the host").unwrap();
        }
        let checkout = Checkout::prepare(directory.path()).unwrap();
        let output = Command::new(tar_program())
            .args(["-tf", "-"])
            .stdin(Stdio::from(checkout.archive.reopen().unwrap()))
            .output()
            .unwrap();
        assert!(output.status.success());
        let names = String::from_utf8(output.stdout).unwrap();
        assert!(names.lines().any(|name| name == "firecrab-api/src/main.rs"));
        assert!(names.lines().any(|name| name == "packaging/m2images.json"));
        assert!(names.lines().any(|name| name == "assets/firecrab-motd"));
        assert!(
            !names
                .lines()
                .any(|name| name == "config.toml" || name.starts_with("firecrab/"))
        );
        assert!(!names.lines().any(|name| {
            name.split('/')
                .any(|part| matches!(part, ".git" | "target" | "node_modules" | ".env"))
        }));
        let output = Command::new(tar_program())
            .args(["-xOf", "-"])
            .stdin(Stdio::from(checkout.archive.reopen().unwrap()))
            .arg("firecrab-api/src/main.rs")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"edited local source");
    }

    #[test]
    fn missing_checkout_fails_before_starting_a_vm() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            Checkout::prepare(directory.path()),
            Err(Error::Checkout { .. })
        ));
    }

    #[test]
    fn malformed_workspace_and_toolchain_are_errors_instead_of_panics() {
        let directory = checkout_fixture();
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname = 'other'\n",
        )
        .unwrap();
        assert!(matches!(
            Checkout::prepare(directory.path()),
            Err(Error::Checkout { .. })
        ));
        let directory = checkout_fixture();
        fs::write(
            directory.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = 'stable; echo injected'\n",
        )
        .unwrap();
        assert!(matches!(
            Checkout::prepare(directory.path()),
            Err(Error::Checkout { .. })
        ));
    }
}
