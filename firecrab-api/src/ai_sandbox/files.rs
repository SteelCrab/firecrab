//! Directory-fd-relative file access: no symlink traversal, absolute paths or credential files.
use std::{
    ffi::CString,
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;
#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

/// Maximum size of any project file admitted to a task.
pub const FILE_LIMIT: usize = 1024 * 1024;
/// Maximum aggregate snapshot size.
pub const TREE_LIMIT: usize = 32 * 1024 * 1024;
/// Maximum file count prevents unbounded walks and archives.
pub const FILE_COUNT: usize = 4096;

/// An opened directory is the authority; later path renames cannot redirect access elsewhere.
pub struct Root {
    /// Opened authority directory; operations resolve each component relative to this fd.
    dir: File,
}

/// Uniform path error avoids returning unrelated host paths.
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Path is outside the project or is reserved",
    )
}
/// Convert one checked path to an OS string without embedded NUL bytes.
fn cstr(value: &str) -> io::Result<CString> {
    CString::new(value).map_err(|_| denied())
}
/// Take ownership of a newly opened fd or preserve the OS error.
fn from_fd(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful openat/open returns a newly owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

/// Reserved paths never enter model context or guest snapshots.
pub fn allowed(path: &str) -> bool {
    if path.is_empty()
        || path.len() > 1000
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return false;
    }
    let mut previous = String::new();
    for part in Path::new(path).components() {
        let Component::Normal(part) = part else {
            return false;
        };
        let Some(part) = part.to_str() else {
            return false;
        };
        let lower = part.to_ascii_lowercase();
        if (previous == ".cargo" && ["registry", "git"].contains(&lower.as_str()))
            || (previous == ".yarn" && lower == "cache")
        {
            return false;
        }
        if [
            ".git",
            ".ssh",
            ".aws",
            ".codex",
            ".claude",
            ".gnupg",
            "node_modules",
            "target",
            ".venv",
            "venv",
            ".next",
            "dist",
            "coverage",
            ".coverage",
            ".cache",
            ".npm",
            ".pnpm-store",
            ".rustup",
            "__pycache__",
            ".pytest_cache",
            ".mypy_cache",
            ".ruff_cache",
        ]
        .contains(&lower.as_str())
            || lower == ".env"
            || lower.starts_with(".env.")
            || lower == "credentials"
            || lower == "credentials.json"
            || lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower == "auth.json"
            || lower == "id_rsa"
            || lower == "id_ed25519"
            || (lower.starts_with("ssh_host_") && lower.ends_with("_key"))
            || [
                ".local",
                ".config",
                ".docker",
                ".netrc",
                ".npmrc",
                ".pypirc",
                ".git-credentials",
                "firecrab.db",
            ]
            .contains(&lower.as_str())
            || lower.starts_with(".firecrab-")
            || lower.ends_with(".secrets")
            || lower.ends_with(".sandbox")
        {
            return false;
        }
        previous = lower;
    }
    true
}

/// Keep project ownership when the privileged API atomically replaces a user's source file.
fn preserve_owner(file: &File, owner: &fs::Metadata) -> io::Result<()> {
    let current = file.metadata()?;
    if current.uid() == owner.uid() && current.gid() == owner.gid() {
        return Ok(());
    }
    // SAFETY: the descriptor is owned; uid/gid come from the original file or opened parent.
    if unsafe { libc::fchown(file.as_raw_fd(), owner.uid(), owner.gid()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Root {
    /// Open a trusted, explicit root directory without following its final symlink.
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = cstr(path.to_str().ok_or_else(denied)?)?;
        // SAFETY: valid NUL-terminated path and standard open flags.
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        Ok(Self { dir: from_fd(fd)? })
    }
    /// Walk parent directories with no-follow flags and return the final relative name.
    fn parent(&self, path: &str, create: bool) -> io::Result<(File, CString)> {
        if !allowed(path) {
            return Err(denied());
        }
        let mut parts = path.split('/').peekable();
        let mut dir = self.dir.try_clone()?;
        while let Some(part) = parts.next() {
            let name = cstr(part)?;
            if parts.peek().is_none() {
                return Ok((dir, name));
            }
            let created = if create {
                // SAFETY: dir is an open directory, name contains one path component.
                let result = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) };
                if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                    return Err(io::Error::last_os_error());
                }
                result == 0
            } else {
                false
            };
            // SAFETY: each directory is opened relative to the previous fd; symlinks are rejected.
            let next = from_fd(unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                )
            })?;
            if created {
                use std::os::unix::fs::PermissionsExt;
                let parent = dir.metadata()?;
                preserve_owner(&next, &parent)?;
                next.set_permissions(fs::Permissions::from_mode(
                    parent.permissions().mode() & 0o755,
                ))?;
            }
            dir = next;
        }
        Err(denied())
    }
    /// Read a bounded regular file through the confined descriptor chain.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let (parent, name) = self.parent(path, false)?;
        // SAFETY: the final path component is relative to an owned directory descriptor.
        let file = from_fd(unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        })?;
        if !file.metadata()?.is_file() || file.metadata()?.nlink() != 1 {
            return Err(denied());
        }
        let mut bytes = Vec::new();
        file.take((FILE_LIMIT + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > FILE_LIMIT {
            return Err(io::Error::other("File exceeds 1 MiB"));
        }
        Ok(bytes)
    }
    /// Read portable permissions from a confined regular file, excluding privilege bits.
    pub fn mode(&self, path: &str) -> io::Result<u32> {
        use std::os::unix::fs::PermissionsExt;
        Ok(self.metadata(path)?.permissions().mode() & 0o777)
    }
    /// Inspect the original regular file without following links or opening special files.
    fn metadata(&self, path: &str) -> io::Result<fs::Metadata> {
        let (parent, name) = self.parent(path, false)?;
        // SAFETY: a regular file is opened without symlink traversal.
        let file = from_fd(unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        })?;
        if !file.metadata()?.is_file() || file.metadata()?.nlink() != 1 {
            return Err(denied());
        }
        file.metadata()
    }
    /// Set portable owner permissions and sanitized executable bits on one confined regular file.
    pub fn set_mode(&self, path: &str, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let (parent, name) = self.parent(path, false)?;
        // SAFETY: chmod operates on an opened regular file, not a mutable path.
        let file = from_fd(unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
        })?;
        if !file.metadata()?.is_file() || file.metadata()?.nlink() != 1 {
            return Err(denied());
        }
        file.set_permissions(fs::Permissions::from_mode(mode & 0o777))
    }
    /// Atomically replace a regular file. Neither parent nor destination symlinks are followed.
    pub fn write(&self, path: &str, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > FILE_LIMIT {
            return Err(io::Error::other("File exceeds 1 MiB"));
        }
        let (parent, name) = self.parent(path, true)?;
        // Opening metadata first rejects symlinks, directories and special files.
        let original = match self.metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let temporary = cstr(&format!(".firecrab-write-{}", Uuid::new_v4()))?;
        // SAFETY: a fresh exclusive file is created in the same anchored directory.
        let mut file = from_fd(unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        })?;
        let result = (|| {
            file.write_all(bytes)?;
            {
                use std::os::unix::fs::PermissionsExt;
                let parent_metadata = parent.metadata()?;
                preserve_owner(&file, original.as_ref().unwrap_or(&parent_metadata))?;
                file.set_permissions(fs::Permissions::from_mode(
                    original
                        .as_ref()
                        .map_or(0o600, |m| m.permissions().mode() & 0o777),
                ))?;
            }
            file.sync_all()?;
            // SAFETY: both names are single components under the same directory descriptor.
            if unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temporary.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: only the newly created temporary name can be removed.
            unsafe { libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0) };
        }
        result
    }
    /// Remove one regular file without following links.
    pub fn remove(&self, path: &str) -> io::Result<()> {
        self.read(path)?;
        let (parent, name) = self.parent(path, false)?;
        // SAFETY: no directory removal flags; the validated regular-file name is confined.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// List bounded regular files, excluding symlinks, build output and credentials.
    pub fn files(&self) -> io::Result<Vec<String>> {
        let mut files = Vec::new();
        self.walk(&self.dir, "", &mut files, 0)?;
        files.sort();
        Ok(files)
    }
    /// Enumerate bounded regular files through already-open directory descriptors.
    fn walk(
        &self,
        dir: &File,
        prefix: &str,
        files: &mut Vec<String>,
        depth: usize,
    ) -> io::Result<()> {
        if depth > 32 {
            return Err(io::Error::other("Project directory depth exceeds 32"));
        }
        // /proc resolves an already-open directory fd, never an untrusted project symlink.
        let directory = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if !allowed(&path) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                let component = cstr(&name)?;
                // SAFETY: recursion opens a child directory with O_NOFOLLOW.
                let child = from_fd(unsafe {
                    libc::openat(
                        dir.as_raw_fd(),
                        component.as_ptr(),
                        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                    )
                })?;
                self.walk(&child, &path, files, depth + 1)?;
            } else if kind.is_file() {
                if files.len() >= FILE_COUNT {
                    return Err(io::Error::other("Project exceeds 4096 files"));
                }
                files.push(path);
            }
        }
        Ok(())
    }
}

/// Copy a bounded snapshot. All destination paths are confined independently.
pub fn snapshot(source: &Path, destination: &Path) -> io::Result<()> {
    snapshot_excluding(source, destination, &[], &[])
}

/// Exclude the actual configured private database and sidecars even under a custom filename.
pub fn snapshot_excluding(
    source_path: &Path,
    destination: &Path,
    excluded: &[PathBuf],
    credentials: &[String],
) -> io::Result<()> {
    let source = Root::open(source_path)?;
    let destination = Root::open(destination)?;
    let mut total = 0;
    for path in source.files()? {
        if excluded
            .iter()
            .any(|p| source_path.join(&path).starts_with(p))
        {
            continue;
        }
        let bytes = source.read(&path)?;
        if credentials
            .iter()
            .filter(|v| !v.is_empty())
            .any(|v| bytes.windows(v.len()).any(|part| part == v.as_bytes()))
        {
            continue;
        }
        total += bytes.len();
        if total > TREE_LIMIT {
            return Err(io::Error::other("Project snapshot exceeds 32 MiB"));
        }
        destination.write(&path, &bytes)?;
        destination.set_mode(&path, 0o600 | (source.mode(&path)? & 0o111))?;
    }
    Ok(())
}
