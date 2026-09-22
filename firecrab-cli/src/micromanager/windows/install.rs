use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use super::super::artifact::{self, ArtifactSpec, HashAlgorithm};

pub const DISTRO_NAME: &str = "firecrab-debian";
pub const DEBIAN_ROOTFS_VERSION: &str = "1.26.0.0";
pub const FIRECRACKER_VERSION: &str = "v1.17.0";
pub const FIRECRAB_VERSION: &str = "v0.2.2";

/// Path of the Firecracker binary inside its release tarball.
const FIRECRACKER_MEMBER: &str = "release-v1.17.0-x86_64/firecracker-v1.17.0-x86_64";

/// Pinned exactly like the macOS provisioner. The rootfs is the build Microsoft
/// publishes for `wsl --install -d Debian`, so it is the same image users get.
const ARTIFACTS: [ArtifactSpec; 3] = [
    ArtifactSpec {
        label: "Debian 13 WSL rootfs",
        filename: "Debian_WSL_AMD64_v1.26.0.0.wsl",
        url: "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/Debian_WSL_AMD64_v1.26.0.0.wsl",
        algorithm: HashAlgorithm::Sha256,
        digest: "5ec7dc68216e75d1d4d4761474e99d8461a98d316537110314b137122a879e0f",
    },
    ArtifactSpec {
        label: "Firecracker x86_64 release",
        filename: "firecracker-v1.17.0-x86_64.tgz",
        url: "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz",
        algorithm: HashAlgorithm::Sha256,
        digest: "06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558",
    },
    ArtifactSpec {
        label: "Firecrab x86_64 GNU host bundle",
        filename: "firecrab-host-x86_64-gnu.tar.gz",
        url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/firecrab-host-x86_64-gnu.tar.gz",
        algorithm: HashAlgorithm::Sha256,
        digest: "ad59b359ccc6d4f28ca6927339b6157b7684b9d734ded79c639a546d709b5315",
    },
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the host is not ready; run `firecrab service doctor` and fix the FAILED checks")]
    NotReady,
    #[error("LOCALAPPDATA is not set; set FIRECRAB_MANAGED_HOME to choose the managed directory")]
    MissingManagedHome,
    #[error("could not use {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Artifact(#[from] artifact::Error),
    #[error("could not run wsl.exe: {source}")]
    Spawn {
        #[source]
        source: io::Error,
    },
    #[error("`wsl {command}` failed: {detail}")]
    Wsl { command: String, detail: String },
    #[error("{path} is not on a drive WSL can reach through /mnt")]
    UnmappablePath { path: PathBuf },
}

pub struct Layout {
    pub managed_home: PathBuf,
}

impl Layout {
    fn from_process_env() -> Result<Self, Error> {
        Self::resolve(
            std::env::var_os("FIRECRAB_MANAGED_HOME"),
            std::env::var_os("LOCALAPPDATA"),
        )
    }

    fn resolve(
        managed_home: Option<OsString>,
        local_app_data: Option<OsString>,
    ) -> Result<Self, Error> {
        if let Some(home) = managed_home.filter(|value| !value.is_empty()) {
            return Ok(Self {
                managed_home: PathBuf::from(home),
            });
        }
        let base = local_app_data
            .filter(|value| !value.is_empty())
            .ok_or(Error::MissingManagedHome)?;
        Ok(Self {
            managed_home: PathBuf::from(base).join("firecrab"),
        })
    }

    fn artifacts_dir(&self) -> PathBuf {
        self.managed_home.join("artifacts")
    }

    fn distro_dir(&self) -> PathBuf {
        self.managed_home.join("distro")
    }
}

struct Artifacts {
    debian_rootfs: PathBuf,
    firecracker: PathBuf,
    firecrab_host: PathBuf,
}

pub fn run() -> Result<i32, Error> {
    let report = super::report(super::Inputs::live());
    if !report.ready {
        println!("{}", super::render_human(&report));
        return Err(Error::NotReady);
    }

    let layout = Layout::from_process_env()?;
    let artifacts_dir = layout.artifacts_dir();
    create_dir(&artifacts_dir)?;
    let artifacts = download_all(&artifacts_dir)?;

    let imported = ensure_distro(&layout, &artifacts.debian_rootfs)?;
    println!(
        "[PASS] distribution: {DISTRO_NAME} {}",
        if imported {
            "imported"
        } else {
            "already present"
        }
    );

    place_guest_binaries(&artifacts)?;
    println!(
        "[PASS] guest binaries: Firecracker {FIRECRACKER_VERSION}, Firecrab {FIRECRAB_VERSION}"
    );

    let verified = verify_guest()?;
    println!(
        "microManager installed\n  managed data: {}\n  distribution: {DISTRO_NAME} (Debian rootfs {DEBIAN_ROOTFS_VERSION})\n  guest: {}\n  remove with: wsl --unregister {DISTRO_NAME}",
        layout.managed_home.display(),
        verified.lines().collect::<Vec<_>>().join(", ")
    );
    Ok(0)
}

fn download_all(directory: &Path) -> Result<Artifacts, Error> {
    artifact::fetch_all(&ARTIFACTS, directory)?;
    Ok(Artifacts {
        debian_rootfs: directory.join(ARTIFACTS[0].filename),
        firecracker: directory.join(ARTIFACTS[1].filename),
        firecrab_host: directory.join(ARTIFACTS[2].filename),
    })
}

fn ensure_distro(layout: &Layout, rootfs: &Path) -> Result<bool, Error> {
    if distro_installed(&wsl(&["--list", "--quiet"])?, DISTRO_NAME) {
        return Ok(false);
    }
    let directory = layout.distro_dir();
    create_dir(&directory)?;
    wsl(&[
        "--import",
        DISTRO_NAME,
        &directory.to_string_lossy(),
        &rootfs.to_string_lossy(),
        "--version",
        "2",
    ])?;
    Ok(true)
}

fn place_guest_binaries(artifacts: &Artifacts) -> Result<(), Error> {
    let firecracker = wsl_path(&artifacts.firecracker)?;
    let bundle = wsl_path(&artifacts.firecrab_host)?;
    let script = format!(
        "set -e; \
         mkdir -p /opt/firecrab; \
         tar -xzf '{firecracker}' -C /tmp '{FIRECRACKER_MEMBER}'; \
         install -m 0755 '/tmp/{FIRECRACKER_MEMBER}' /usr/local/bin/firecracker; \
         rm -rf /tmp/release-v1.17.0-x86_64; \
         tar -xzf '{bundle}' -C /opt/firecrab; \
         install -m 0755 /opt/firecrab/firecrab /usr/local/bin/firecrab"
    );
    wsl(&["-d", DISTRO_NAME, "-u", "root", "--", "sh", "-c", &script])?;
    Ok(())
}

fn verify_guest() -> Result<String, Error> {
    let script = "firecracker --version 2>&1 | head -1; \
                  firecrab --version 2>&1 | head -1; \
                  (exec 3<>/dev/kvm) 2>/dev/null && echo kvm=open || echo kvm=closed";
    wsl(&["-d", DISTRO_NAME, "-u", "root", "--", "sh", "-c", script])
}

fn wsl(args: &[&str]) -> Result<String, Error> {
    let output = ProcessCommand::new("wsl.exe")
        .args(args)
        .output()
        .map_err(|source| Error::Spawn { source })?;
    if !output.status.success() {
        return Err(Error::Wsl {
            command: args.join(" "),
            detail: super::decode_console(&output.stderr).trim().to_string(),
        });
    }
    Ok(super::decode_console(&output.stdout))
}

fn create_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn distro_installed(list_output: &str, name: &str) -> bool {
    list_output
        .lines()
        .map(str::trim)
        .any(|line| line.eq_ignore_ascii_case(name))
}

/// `C:\Users\dev\x` becomes `/mnt/c/Users/dev/x`, the path the distribution sees.
fn wsl_path(path: &Path) -> Result<String, Error> {
    let unmappable = || Error::UnmappablePath {
        path: path.to_path_buf(),
    };
    let text = path.to_str().ok_or_else(unmappable)?;
    let (drive, rest) = text.split_once(":\\").ok_or_else(unmappable)?;
    let [letter] = drive.as_bytes() else {
        return Err(unmappable());
    };
    if !letter.is_ascii_alphabetic() {
        return Err(unmappable());
    }
    Ok(format!(
        "/mnt/{}/{}",
        letter.to_ascii_lowercase() as char,
        rest.replace('\\', "/")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_home_defaults_under_local_app_data() {
        let local_app_data = "C:\\Users\\dev\\AppData\\Local";
        let layout =
            Layout::resolve(None, Some(OsString::from(local_app_data))).expect("layout resolves");
        assert_eq!(
            layout.managed_home,
            PathBuf::from(local_app_data).join("firecrab")
        );
        assert!(layout.artifacts_dir().ends_with("artifacts"));
        assert!(layout.distro_dir().ends_with("distro"));
    }

    #[test]
    fn an_explicit_managed_home_wins() {
        let layout = Layout::resolve(
            Some(OsString::from("D:\\lab")),
            Some(OsString::from("C:\\Users\\dev\\AppData\\Local")),
        )
        .expect("layout resolves");
        assert_eq!(layout.managed_home, PathBuf::from("D:\\lab"));
    }

    #[test]
    fn an_empty_environment_is_rejected() {
        assert!(matches!(
            Layout::resolve(Some(OsString::new()), Some(OsString::new())),
            Err(Error::MissingManagedHome)
        ));
    }

    #[test]
    fn windows_paths_map_into_the_distribution() {
        assert_eq!(
            wsl_path(Path::new("C:\\Users\\dev\\firecrab\\a.tgz")).expect("mapped"),
            "/mnt/c/Users/dev/firecrab/a.tgz"
        );
        assert_eq!(
            wsl_path(Path::new("D:\\lab\\x")).expect("mapped"),
            "/mnt/d/lab/x"
        );
    }

    #[test]
    fn unc_and_relative_paths_are_rejected() {
        assert!(wsl_path(Path::new("\\\\server\\share\\x")).is_err());
        assert!(wsl_path(Path::new("artifacts\\x.tgz")).is_err());
    }

    #[test]
    fn the_managed_distribution_is_detected_case_insensitively() {
        let listing = "Ubuntu\r\nFIRECRAB-DEBIAN\r\nDebian\r\n";
        assert!(distro_installed(listing, DISTRO_NAME));
        assert!(!distro_installed("Ubuntu\r\nDebian\r\n", DISTRO_NAME));
    }

    #[test]
    fn a_user_distribution_named_debian_is_not_the_managed_one() {
        assert!(!distro_installed("Debian\r\n", DISTRO_NAME));
    }

    #[test]
    fn every_artifact_is_pinned_to_a_digest() {
        for spec in &ARTIFACTS {
            assert_eq!(spec.digest.len(), 64, "{} digest length", spec.label);
            assert!(spec.url.starts_with("https://"), "{} scheme", spec.label);
            assert!(spec.digest.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
