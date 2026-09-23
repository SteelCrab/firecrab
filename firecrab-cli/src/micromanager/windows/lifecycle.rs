//! Where the Windows backend keeps its state, and removing it again.
//!
//! The layout mirrors macOS (`downloads/`, `provision/`, `runtime/` under a
//! directory named `micromanager`) so the purge rules and docs apply to both.
//! The WSL distribution's own disk lives in `distro/`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;

use super::super::managed_home;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "LOCALAPPDATA is not set; set FIRECRAB_MICROMANAGER_HOME to choose the managed directory"
    )]
    MissingManagedHome,
    #[error(transparent)]
    ManagedHome(#[from] managed_home::Error),
    #[error("could not create {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub managed_home: PathBuf,
}

impl Layout {
    pub fn from_process_env() -> Result<Self, Error> {
        Self::resolve(
            std::env::var_os("FIRECRAB_MICROMANAGER_HOME"),
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
            managed_home: PathBuf::from(base).join("Firecrab").join("micromanager"),
        })
    }

    /// Verified immutable release artifacts.
    pub fn downloads(&self) -> PathBuf {
        self.managed_home.join("downloads")
    }

    /// Scripts the guest runs during provisioning.
    pub fn provision(&self) -> PathBuf {
        self.managed_home.join("provision")
    }

    /// Markers, logs, and the scheduled task definition.
    pub fn runtime(&self) -> PathBuf {
        self.managed_home.join("runtime")
    }

    /// The imported distribution's `ext4.vhdx`, owned by WSL.
    pub fn distro(&self) -> PathBuf {
        self.managed_home.join("distro")
    }

    pub fn provision_marker(&self) -> PathBuf {
        self.runtime().join("provisioned")
    }
}

/// Creates the managed directories, refusing a managed home reached through a symlink.
pub fn prepare(layout: &Layout) -> Result<(), Error> {
    managed_home::reject_symlink_components(&layout.managed_home)?;
    for path in [
        layout.downloads(),
        layout.provision(),
        layout.runtime(),
        layout.distro(),
    ] {
        fs::create_dir_all(&path).map_err(|source| Error::CreateDirectory { path, source })?;
    }
    Ok(())
}

/// Checked before anything is unregistered, so an unsafe path stops the purge
/// while the distribution is still intact.
pub fn validate_purge(layout: &Layout) -> Result<(), Error> {
    Ok(managed_home::validate_purge(&layout.managed_home)?)
}

pub fn purge(layout: &Layout) -> Result<(), Error> {
    Ok(managed_home::purge(&layout.managed_home)?)
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
            PathBuf::from(local_app_data)
                .join("Firecrab")
                .join("micromanager")
        );
        assert!(layout.downloads().ends_with("downloads"));
        assert!(layout.provision().ends_with("provision"));
        assert!(layout.distro().ends_with("distro"));
        assert!(layout.provision_marker().ends_with("runtime/provisioned"));
    }

    #[test]
    fn an_explicit_managed_home_wins() {
        let layout = Layout::resolve(
            Some(OsString::from("D:\\lab\\micromanager")),
            Some(OsString::from("C:\\Users\\dev\\AppData\\Local")),
        )
        .expect("layout resolves");
        assert_eq!(layout.managed_home, PathBuf::from("D:\\lab\\micromanager"));
    }

    #[test]
    fn an_empty_environment_is_rejected() {
        assert!(matches!(
            Layout::resolve(Some(OsString::new()), Some(OsString::new())),
            Err(Error::MissingManagedHome)
        ));
    }

    #[test]
    fn prepare_creates_every_managed_directory_and_purge_removes_them() {
        let directory = tempfile::tempdir().expect("temp dir");
        let layout = Layout {
            managed_home: directory.path().join("Firecrab").join("micromanager"),
        };
        prepare(&layout).expect("directories are created");
        for path in [
            layout.downloads(),
            layout.provision(),
            layout.runtime(),
            layout.distro(),
        ] {
            assert!(path.is_dir(), "{} exists", path.display());
        }
        validate_purge(&layout).expect("a managed home may be purged");
        purge(&layout).expect("purge succeeds");
        assert!(!layout.managed_home.exists());
    }

    #[test]
    fn a_managed_home_not_named_micromanager_is_never_purged() {
        let directory = tempfile::tempdir().expect("temp dir");
        let layout = Layout {
            managed_home: directory.path().join("firecrab"),
        };
        prepare(&layout).expect("directories are created");
        assert!(matches!(
            validate_purge(&layout),
            Err(Error::ManagedHome(managed_home::Error::UnsafePurge(_)))
        ));
        assert!(layout.managed_home.is_dir());
    }
}
