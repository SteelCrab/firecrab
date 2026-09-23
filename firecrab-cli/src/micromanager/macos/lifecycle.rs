use std::ffi::OsString;
use std::fs::{self, File, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::super::managed_home;

const CLI_NAME: &str = "firecrab";
const HELPER_NAME: &str = "firecrab-micromanager-macos";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    pub install_dir: PathBuf,
    pub managed_home: PathBuf,
}

impl Layout {
    pub fn from_process_env() -> Result<Self, Error> {
        Self::from_values(
            std::env::var_os("HOME"),
            std::env::var_os("FIRECRAB_INSTALL_DIR"),
            std::env::var_os("FIRECRAB_MICROMANAGER_HOME"),
        )
    }

    fn from_values(
        home: Option<OsString>,
        install_dir: Option<OsString>,
        managed_home: Option<OsString>,
    ) -> Result<Self, Error> {
        let home = home
            .filter(|value| !value.is_empty())
            .ok_or(Error::MissingHome)?;
        let home = absolute(PathBuf::from(home))?;
        let install_dir = install_dir
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/bin"));
        let managed_home = managed_home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Library/Application Support/Firecrab/micromanager"));
        Ok(Self {
            install_dir: absolute(install_dir)?,
            managed_home: absolute(managed_home)?,
        })
    }

    pub fn cli_path(&self) -> PathBuf {
        self.install_dir.join(CLI_NAME)
    }

    pub fn helper_path(&self) -> PathBuf {
        self.install_dir.join(HELPER_NAME)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HOME is not set; set HOME or the explicit install and managed-home variables")]
    MissingHome,
    #[error("cannot resolve an absolute path: {0}")]
    CurrentDirectory(#[source] io::Error),
    #[error("installation source is not a regular file: {0}")]
    InvalidSource(PathBuf),
    #[error("microManager is already installed at {0}; use `firecrab service reinstall`")]
    AlreadyInstalled(PathBuf),
    #[error("refusing to replace a directory: {0}")]
    TargetDirectory(PathBuf),
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

pub fn install_from(
    cli_source: &Path,
    helper_source: &Path,
    layout: &Layout,
    reinstall: bool,
) -> Result<(), Error> {
    validate_source(cli_source)?;
    validate_source(helper_source)?;
    fs::create_dir_all(&layout.install_dir)
        .map_err(|source| io_error("create install directory", &layout.install_dir, source))?;

    let cli_target = layout.cli_path();
    let helper_target = layout.helper_path();
    validate_target(&cli_target, reinstall)?;
    validate_target(&helper_target, reinstall)?;

    let cli_staged = stage_binary(cli_source, &layout.install_dir)?;
    let helper_staged = stage_binary(helper_source, &layout.install_dir)?;
    ensure_managed_directories(&layout.managed_home)?;

    helper_staged
        .persist(&helper_target)
        .map_err(|error| io_error("install helper", &helper_target, error.error))?;
    cli_staged
        .persist(&cli_target)
        .map_err(|error| io_error("install CLI", &cli_target, error.error))?;
    Ok(())
}

pub fn uninstall_at(layout: &Layout, purge: bool) -> Result<(), Error> {
    if purge {
        managed_home::validate_purge(&layout.managed_home)?;
    }
    remove_binary(&layout.cli_path())?;
    remove_binary(&layout.helper_path())?;
    if purge {
        managed_home::purge(&layout.managed_home)?;
    }
    Ok(())
}

fn validate_source(path: &Path) -> Result<(), Error> {
    let metadata = fs::metadata(path).map_err(|source| io_error("read source", path, source))?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(Error::InvalidSource(path.to_owned()))
    }
}

fn validate_target(path: &Path, reinstall: bool) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(Error::TargetDirectory(path.to_owned())),
        Ok(_) if !reinstall => Err(Error::AlreadyInstalled(path.to_owned())),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect target", path, source)),
    }
}

fn stage_binary(source: &Path, install_dir: &Path) -> Result<tempfile::NamedTempFile, Error> {
    let mut source_file =
        File::open(source).map_err(|error| io_error("open source", source, error))?;
    let mut staged = tempfile::NamedTempFile::new_in(install_dir)
        .map_err(|error| io_error("create staged binary", install_dir, error))?;
    io::copy(&mut source_file, &mut staged)
        .map_err(|error| io_error("copy staged binary", staged.path(), error))?;
    staged
        .flush()
        .map_err(|error| io_error("flush staged binary", staged.path(), error))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|error| io_error("sync staged binary", staged.path(), error))?;
    staged
        .as_file()
        .set_permissions(Permissions::from_mode(0o755))
        .map_err(|error| io_error("set staged permissions", staged.path(), error))?;
    Ok(staged)
}

fn ensure_managed_directories(managed_home: &Path) -> Result<(), Error> {
    managed_home::reject_symlink_components(managed_home)?;
    for path in [
        managed_home.to_owned(),
        managed_home.join("system"),
        managed_home.join("data"),
    ] {
        fs::create_dir_all(&path)
            .map_err(|source| io_error("create managed directory", &path, source))?;
        fs::set_permissions(&path, Permissions::from_mode(0o700))
            .map_err(|source| io_error("protect managed directory", &path, source))?;
    }
    Ok(())
}

fn remove_binary(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Err(Error::TargetDirectory(path.to_owned())),
        Ok(_) => fs::remove_file(path).map_err(|source| io_error("remove binary", path, source)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect binary", path, source)),
    }
}

fn absolute(path: PathBuf) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(Error::CurrentDirectory)
    }
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

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Layout) {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("source-firecrab");
        let helper = directory.path().join("source-helper");
        fs::write(&cli, b"cli-v1").unwrap();
        fs::write(&helper, b"helper-v1").unwrap();
        let layout = Layout {
            install_dir: directory.path().join("bin"),
            managed_home: directory.path().join("state/micromanager"),
        };
        (directory, cli, helper, layout)
    }

    #[test]
    fn install_reinstall_and_uninstall_preserve_managed_data() {
        let (_directory, cli, helper, layout) = fixture();
        install_from(&cli, &helper, &layout, false).unwrap();
        assert_eq!(fs::read(layout.cli_path()).unwrap(), b"cli-v1");
        assert_eq!(fs::read(layout.helper_path()).unwrap(), b"helper-v1");
        assert!(layout.managed_home.join("system").is_dir());
        assert!(layout.managed_home.join("data").is_dir());
        assert_eq!(
            fs::metadata(layout.managed_home.join("data"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let sentinel = layout.managed_home.join("data/keep-me");
        fs::write(&sentinel, b"persistent").unwrap();
        fs::write(&cli, b"cli-v2").unwrap();
        fs::write(&helper, b"helper-v2").unwrap();
        install_from(&cli, &helper, &layout, true).unwrap();
        assert_eq!(fs::read(layout.cli_path()).unwrap(), b"cli-v2");
        assert_eq!(fs::read(layout.helper_path()).unwrap(), b"helper-v2");
        assert_eq!(fs::read(&sentinel).unwrap(), b"persistent");

        uninstall_at(&layout, false).unwrap();
        assert!(!layout.cli_path().exists());
        assert!(!layout.helper_path().exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"persistent");
    }

    #[test]
    fn plain_install_refuses_an_existing_install() {
        let (_directory, cli, helper, layout) = fixture();
        install_from(&cli, &helper, &layout, false).unwrap();
        assert!(matches!(
            install_from(&cli, &helper, &layout, false),
            Err(Error::AlreadyInstalled(_))
        ));
    }

    #[test]
    fn explicit_purge_removes_data_and_rejects_unsafe_roots() {
        let (_directory, cli, helper, layout) = fixture();
        install_from(&cli, &helper, &layout, false).unwrap();
        fs::write(layout.managed_home.join("data/delete-me"), b"data").unwrap();
        uninstall_at(&layout, true).unwrap();
        assert!(!layout.managed_home.exists());

        let (_unsafe_directory, unsafe_cli, unsafe_helper, installed_layout) = fixture();
        install_from(&unsafe_cli, &unsafe_helper, &installed_layout, false).unwrap();
        let unsafe_layout = Layout {
            install_dir: installed_layout.install_dir.clone(),
            managed_home: PathBuf::from("/tmp"),
        };
        assert!(matches!(
            uninstall_at(&unsafe_layout, true),
            Err(Error::ManagedHome(managed_home::Error::UnsafePurge(_)))
        ));
        assert!(installed_layout.cli_path().is_file());
        assert!(installed_layout.helper_path().is_file());
    }

    #[test]
    fn purge_rejects_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real_parent = directory.path().join("real");
        let real_managed_home = real_parent.join("micromanager");
        fs::create_dir_all(&real_managed_home).unwrap();
        let linked_parent = directory.path().join("linked");
        symlink(&real_parent, &linked_parent).unwrap();
        let layout = Layout {
            install_dir: directory.path().join("bin"),
            managed_home: linked_parent.join("micromanager"),
        };

        assert!(matches!(
            uninstall_at(&layout, true),
            Err(Error::ManagedHome(managed_home::Error::UnsafeManagedPath(
                _
            )))
        ));
        assert!(real_managed_home.is_dir());
    }

    #[test]
    fn default_layout_matches_the_swift_managed_home() {
        let layout =
            Layout::from_values(Some(OsString::from("/Users/example")), None, None).unwrap();
        assert_eq!(layout.install_dir, Path::new("/Users/example/.local/bin"));
        assert_eq!(
            layout.managed_home,
            Path::new("/Users/example/Library/Application Support/Firecrab/micromanager")
        );
    }
}
