//! Explicit installs select release inputs here. Ordinary service commands
//! keep using the installed guest and never create or migrate this manifest.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::artifact::{ArtifactSpec, HashAlgorithm};
use super::managed_home;

pub const FILE_NAME: &str = "micromanager.toml";
const BUNDLED: &str = include_str!("../../../packaging/micromanager.toml");

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    schema_version: u32,
    pub debian: Debian,
    pub firecracker: Release,
    pub firecrab: Firecrab,
    pub macos: Macos,
    pub windows: Windows,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Debian {
    pub version: u32,
    pub suite: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub version: String,
    pub aarch64: Artifact,
    pub x86_64: Artifact,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Firecrab {
    pub version: String,
    pub installer: Artifact,
    pub aarch64: Artifact,
    pub x86_64: Artifact,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Macos {
    pub image_build: String,
    pub kernel_version: String,
    pub image: Artifact,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Windows {
    pub rootfs_version: String,
    pub aarch64: WindowsImage,
    pub x86_64: WindowsImage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsImage {
    pub kernel_version: String,
    pub image: Artifact,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub filename: String,
    pub url: String,
    pub algorithm: HashAlgorithm,
    pub digest: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid microManager TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid microManager manifest: {0}")]
    Invalid(String),
    #[error("could not encode microManager TOML: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Manifest {
    pub fn bundled() -> Result<Self, Error> {
        Self::parse(BUNDLED)
    }

    fn parse(text: &str) -> Result<Self, Error> {
        let manifest: Self = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Absence means a legacy installation. A broken file must not silently
    /// select the legacy pins and report a TOML installation as healthy.
    pub fn installed(home: &Path) -> Result<Option<Self>, Error> {
        let path = home.join(FILE_NAME);
        managed_home::reject_symlink_components(&path)?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Self::parse(&text)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error("read installed manifest", &path, source)),
        }
    }

    /// Called only after the explicit install's guest provisioning gates pass.
    pub fn save(&self, home: &Path) -> Result<(), Error> {
        self.validate()?;
        let path = home.join(FILE_NAME);
        managed_home::reject_symlink_components(&path)?;
        let mut staged = tempfile::NamedTempFile::new_in(home)
            .map_err(|source| io_error("stage installed manifest", &path, source))?;
        staged
            .write_all(toml::to_string_pretty(self)?.as_bytes())
            .map_err(|source| io_error("write installed manifest", &path, source))?;
        staged
            .as_file()
            .sync_all()
            .map_err(|source| io_error("sync installed manifest", &path, source))?;
        staged
            .persist(&path)
            .map_err(|error| io_error("publish installed manifest", &path, error.error))?;
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String, Error> {
        Ok(format!("{:x}", Sha256::digest(toml::to_string(self)?)))
    }

    pub fn matches_marker(&self, marker: &str) -> Result<bool, Error> {
        Ok(marker_value_matches(
            marker,
            "manifest",
            &self.fingerprint()?,
        ))
    }

    pub fn verify_provisioned(
        &self,
        marker: &str,
        kernel_key: &str,
        kernel: &str,
    ) -> Result<(), Error> {
        if !self.matches_marker(marker)? || !marker_value_matches(marker, kernel_key, kernel) {
            return Err(Error::Invalid(format!(
                "guest did not apply the selected TOML manifest and {kernel_key}={kernel}"
            )));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn macos_artifacts(&self) -> [ArtifactSpec<'_>; 4] {
        [
            self.macos.image.spec("Debian ARM64 management image"),
            self.firecracker.aarch64.spec("Firecracker ARM64 release"),
            self.firecrab.aarch64.spec("Firecrab ARM64 GNU host bundle"),
            self.firecrab.installer.spec("Firecrab guest installer"),
        ]
    }

    fn validate(&self) -> Result<(), Error> {
        if self.schema_version != 1 {
            return Err(Error::Invalid(format!(
                "unsupported schema {}",
                self.schema_version
            )));
        }
        if self.debian.version == 0 {
            return Err(Error::Invalid("Debian version must be positive".into()));
        }
        validate_token("Debian suite", &self.debian.suite)?;
        for (name, value) in [
            ("Firecracker version", &self.firecracker.version),
            ("Firecrab version", &self.firecrab.version),
            ("macOS image build", &self.macos.image_build),
            ("Windows rootfs version", &self.windows.rootfs_version),
        ] {
            validate_token(name, value)?;
        }
        for (version, architecture) in [
            (&self.macos.kernel_version, "arm64"),
            (&self.windows.aarch64.kernel_version, "arm64"),
            (&self.windows.x86_64.kernel_version, "amd64"),
        ] {
            validate_token("kernel version", version)?;
            let suffix = format!("+deb{}-{architecture}", self.debian.version);
            if !version.ends_with(&suffix) {
                return Err(Error::Invalid(format!(
                    "kernel {version} must end with {suffix}"
                )));
            }
        }
        for artifact in [
            &self.macos.image,
            &self.windows.aarch64.image,
            &self.windows.x86_64.image,
            &self.firecracker.aarch64,
            &self.firecracker.x86_64,
            &self.firecrab.aarch64,
            &self.firecrab.x86_64,
            &self.firecrab.installer,
        ] {
            artifact.validate()?;
        }
        Ok(())
    }
}

impl Artifact {
    pub fn spec(&self, label: &'static str) -> ArtifactSpec<'_> {
        ArtifactSpec {
            label,
            filename: &self.filename,
            url: &self.url,
            algorithm: self.algorithm,
            digest: &self.digest,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        validate_token("artifact filename", &self.filename)?;
        if self.filename == "." || self.filename == ".." {
            return Err(Error::Invalid("artifact filename must name a file".into()));
        }
        let url = reqwest::Url::parse(&self.url)
            .map_err(|error| Error::Invalid(format!("artifact URL: {error}")))?;
        if url.scheme() != "https" || url.host_str().is_none() {
            return Err(Error::Invalid("artifact URL must use HTTPS".into()));
        }
        let length = match self.algorithm {
            HashAlgorithm::Sha256 => 64,
            HashAlgorithm::Sha512 => 128,
        };
        if self.digest.len() != length || !self.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Error::Invalid(format!(
                "invalid checksum for {}",
                self.filename
            )));
        }
        Ok(())
    }
}

fn validate_token(name: &str, value: &str) -> Result<(), Error> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        return Err(Error::Invalid(format!("invalid {name}: {value:?}")));
    }
    Ok(())
}

pub fn marker_value_matches(marker: &str, key: &str, expected: &str) -> bool {
    let mut values = marker.lines().filter_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name == key).then_some(value)
    });
    values.next() == Some(expected) && values.next().is_none()
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_a_legacy_install_does_not_create_a_manifest_or_change_its_data() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("data.raw"), b"keep").unwrap();
        assert!(Manifest::installed(home.path()).unwrap().is_none());
        assert!(!home.path().join(FILE_NAME).exists());
        assert_eq!(fs::read(home.path().join("data.raw")).unwrap(), b"keep");
    }

    #[test]
    fn a_saved_manifest_keeps_its_identity_and_does_not_accept_legacy_markers() {
        let home = tempfile::tempdir().unwrap();
        let manifest = Manifest::bundled().unwrap();
        assert!(!manifest.matches_marker("schema=5\nkernel=old\n").unwrap());
        manifest.save(home.path()).unwrap();
        let installed = Manifest::installed(home.path()).unwrap().unwrap();
        assert_eq!(
            installed.fingerprint().unwrap(),
            manifest.fingerprint().unwrap()
        );
        let marker = format!("manifest={}\n", manifest.fingerprint().unwrap());
        assert!(installed.matches_marker(&marker).unwrap());
        assert!(
            installed
                .verify_provisioned(
                    &format!("{marker}kernel=old\n"),
                    "kernel",
                    &installed.macos.kernel_version
                )
                .is_err()
        );
        assert!(
            installed
                .verify_provisioned(
                    &format!("{marker}kernel={}\n", installed.macos.kernel_version),
                    "kernel",
                    &installed.macos.kernel_version
                )
                .is_ok()
        );
        assert!(
            !installed
                .matches_marker(&format!("{marker}{marker}"))
                .unwrap()
        );
    }

    #[test]
    fn invalid_manifest_inputs_and_corrupt_installed_files_are_errors() {
        for text in [
            BUNDLED.replace("schema_version = 1", "schema_version = 2"),
            BUNDLED.replace("install-firecrab-v0.2.2.sh", "../escape.sh"),
            BUNDLED.replace("algorithm = \"sha256\"", "algorithm = \"md5\""),
            BUNDLED.replace("6.12.111+deb13-arm64", "6.12.111;touch-pwned"),
            BUNDLED.replace("https://", "http://"),
            format!("unknown = true\n{BUNDLED}"),
        ] {
            assert!(Manifest::parse(&text).is_err());
        }
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join(FILE_NAME), "broken = [").unwrap();
        assert!(Manifest::installed(home.path()).is_err());
        assert_eq!(
            fs::read_to_string(home.path().join(FILE_NAME)).unwrap(),
            "broken = ["
        );
    }
}
