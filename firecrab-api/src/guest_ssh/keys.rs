//! Host-side operator and guest-host key files.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::artifacts::VmArtifactPaths;

const SSH_KEYGEN: &str = "ssh-keygen";

/// Host-side SSH files for one VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSshPaths {
    /// `{vms}/{id}/ssh/`.
    pub dir: PathBuf,
    /// Operator private key (`ssh -i`).
    pub operator_private: PathBuf,
    /// Operator public key (authorized_keys).
    pub operator_public: PathBuf,
    /// Guest host private key.
    pub host_private: PathBuf,
    /// Guest host public key (fingerprint).
    pub host_public: PathBuf,
}

impl VmSshPaths {
    /// Layout under the VM artifact tree.
    pub fn from_artifacts(paths: &VmArtifactPaths) -> Self {
        let dir = paths.dir.join("ssh");
        Self {
            operator_private: dir.join("id_ed25519"),
            operator_public: dir.join("id_ed25519.pub"),
            host_private: dir.join("ssh_host_ed25519_key"),
            host_public: dir.join("ssh_host_ed25519_key.pub"),
            dir,
        }
    }
}

/// Why key generation or install failed.
#[derive(Debug, Error)]
pub enum SshError {
    /// `ssh-keygen` missing or failed.
    #[error("ssh-keygen failed for {path}: {detail}")]
    Keygen { path: PathBuf, detail: String },
    /// Could not create or chmod the ssh directory.
    #[error("failed to prepare {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// `firecrab-<name>.pem` with non-portable characters replaced.
pub fn pem_filename(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "vm".to_owned()
    } else {
        safe
    };
    format!("firecrab-{safe}.pem")
}

/// Creates `{dir}/ssh` and the operator ed25519 pair if missing.
pub fn ensure_operator_key(paths: &VmArtifactPaths) -> Result<VmSshPaths, SshError> {
    let ssh = VmSshPaths::from_artifacts(paths);
    fs::create_dir_all(&ssh.dir).map_err(|source| SshError::Io {
        path: ssh.dir.clone(),
        source,
    })?;
    chmod(&ssh.dir, 0o700)?;
    if !ssh.operator_private.is_file() {
        keygen(
            &ssh.operator_private,
            &format!("firecrab-operator-{}", uuid_comment(paths)),
        )?;
    }
    chmod(&ssh.operator_private, 0o600)?;
    Ok(ssh)
}

/// Moves `{from}/ssh` onto `{to}/ssh` when an operator key already exists.
///
/// Storage reassignment is allowed before the first disk exists, but the
/// operator key is created at VM create. Leaving it on the old root would
/// make `GET /ssh-key` miss the file (or mint a second key on next start).
pub fn relocate_ssh_artifacts(
    from: &VmArtifactPaths,
    to: &VmArtifactPaths,
) -> Result<(), SshError> {
    let src = VmSshPaths::from_artifacts(from);
    if !src.dir.is_dir() {
        return Ok(());
    }
    to.ensure_directories().map_err(|source| SshError::Io {
        path: to.dir.clone(),
        source: io::Error::other(source.to_string()),
    })?;
    let dst = VmSshPaths::from_artifacts(to);
    if src.dir == dst.dir {
        return Ok(());
    }
    if dst.dir.exists() {
        fs::remove_dir_all(&dst.dir).map_err(|source| SshError::Io {
            path: dst.dir.clone(),
            source,
        })?;
    }
    match fs::rename(&src.dir, &dst.dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_tree(&src.dir, &dst.dir)?;
            fs::remove_dir_all(&src.dir).map_err(|source| SshError::Io {
                path: src.dir.clone(),
                source,
            })
        }
        Err(source) => Err(SshError::Io {
            path: src.dir,
            source,
        }),
    }
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), SshError> {
    fs::create_dir_all(dst).map_err(|source| SshError::Io {
        path: dst.to_owned(),
        source,
    })?;
    chmod(dst, 0o700)?;
    for entry in fs::read_dir(src).map_err(|source| SshError::Io {
        path: src.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| SshError::Io {
            path: src.to_owned(),
            source,
        })?;
        let to = dst.join(entry.file_name());
        let from = entry.path();
        let file_type = entry.file_type().map_err(|source| SshError::Io {
            path: from.clone(),
            source,
        })?;
        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|source| SshError::Io {
                path: to.clone(),
                source,
            })?;
            if let Ok(mode) = fs::metadata(&from).map(|meta| meta.permissions().mode()) {
                let _ = chmod(&to, mode & 0o777);
            }
        }
    }
    Ok(())
}

/// Operator pair plus a stable guest host key pair.
pub fn ensure_vm_ssh_keys(paths: &VmArtifactPaths) -> Result<VmSshPaths, SshError> {
    let ssh = ensure_operator_key(paths)?;
    if !ssh.host_private.is_file() {
        keygen(&ssh.host_private, "firecrab-host")?;
    }
    chmod(&ssh.host_private, 0o600)?;
    Ok(ssh)
}

/// `SHA256:…` fingerprint of the guest host public key, if generated.
pub fn host_fingerprint(paths: &VmArtifactPaths) -> Option<String> {
    let ssh = VmSshPaths::from_artifacts(paths);
    fingerprint(&ssh.host_public)
}

fn uuid_comment(paths: &VmArtifactPaths) -> String {
    paths
        .dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("vm")
        .to_owned()
}

fn chmod(path: &Path, mode: u32) -> Result<(), SshError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| SshError::Io {
        path: path.to_owned(),
        source,
    })
}

fn keygen(private: &Path, comment: &str) -> Result<(), SshError> {
    if let Some(parent) = private.parent() {
        fs::create_dir_all(parent).map_err(|source| SshError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let output = Command::new(SSH_KEYGEN)
        .args(["-t", "ed25519", "-N", "", "-q", "-C", comment, "-f"])
        .arg(private)
        .output()
        .map_err(|source| SshError::Keygen {
            path: private.to_owned(),
            detail: source.to_string(),
        })?;
    if !output.status.success() {
        return Err(SshError::Keygen {
            path: private.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

fn fingerprint(public: &Path) -> Option<String> {
    let output = Command::new(SSH_KEYGEN)
        .args(["-lf"])
        .arg(public)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    line.split_whitespace()
        .find(|part| part.starts_with("SHA256:"))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn artifacts(dir: &Path) -> VmArtifactPaths {
        let paths = VmArtifactPaths::for_vm(dir, Uuid::from_u128(1));
        paths.ensure_directories().unwrap();
        paths
    }

    #[test]
    fn pem_filename_sanitizes_spaces_and_slashes() {
        assert_eq!(pem_filename("web app"), "firecrab-web-app.pem");
        assert_eq!(pem_filename("a/b"), "firecrab-a-b.pem");
        assert_eq!(pem_filename("ok_1.2"), "firecrab-ok_1.2.pem");
        assert_eq!(pem_filename(""), "firecrab-vm.pem");
    }

    #[test]
    fn ensure_operator_key_writes_an_ed25519_pair_once() {
        let directory = tempdir().unwrap();
        let paths = artifacts(directory.path());
        let first = ensure_operator_key(&paths).unwrap();
        let pub1 = fs::read(&first.operator_public).unwrap();
        assert!(first.operator_private.is_file());
        assert!(
            std::str::from_utf8(&pub1)
                .unwrap()
                .starts_with("ssh-ed25519 ")
        );
        let mode = fs::metadata(&first.operator_private)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        let second = ensure_operator_key(&paths).unwrap();
        let pub2 = fs::read(&second.operator_public).unwrap();
        assert_eq!(pub1, pub2);
    }

    #[test]
    fn ensure_vm_ssh_keys_makes_a_stable_host_fingerprint() {
        let directory = tempdir().unwrap();
        let paths = artifacts(directory.path());
        ensure_vm_ssh_keys(&paths).unwrap();
        let fp = host_fingerprint(&paths).expect("fingerprint");
        assert!(fp.starts_with("SHA256:"), "{fp}");
        ensure_vm_ssh_keys(&paths).unwrap();
        assert_eq!(host_fingerprint(&paths).as_deref(), Some(fp.as_str()));
    }

    #[test]
    fn relocate_ssh_artifacts_moves_the_operator_key() {
        let directory = tempdir().unwrap();
        let from = artifacts(&directory.path().join("from"));
        let to = artifacts(&directory.path().join("to"));
        let first = ensure_operator_key(&from).unwrap();
        let bytes = fs::read(&first.operator_private).unwrap();

        relocate_ssh_artifacts(&from, &to).unwrap();

        assert!(!first.operator_private.is_file());
        let moved = VmSshPaths::from_artifacts(&to);
        assert_eq!(fs::read(&moved.operator_private).unwrap(), bytes);
    }

    #[test]
    fn relocate_ssh_artifacts_is_a_no_op_when_nothing_was_generated() {
        let directory = tempdir().unwrap();
        let from = artifacts(&directory.path().join("from"));
        let to = artifacts(&directory.path().join("to"));
        relocate_ssh_artifacts(&from, &to).unwrap();
        assert!(!VmSshPaths::from_artifacts(&to).dir.exists());
    }
}
