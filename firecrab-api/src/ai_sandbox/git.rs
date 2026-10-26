//! Git reads only a bounded private bare snapshot, never the authorized source's live metadata.
use super::files::Root;
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};
use uuid::Uuid;

/// History is bounded separately from the 32 MiB exported source tree.
const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 32768;

fn invalid(message: &'static str) -> io::Error {
    io::Error::other(message)
}
fn unsafe_metadata() -> io::Error {
    invalid(
        "Git metadata must use owned regular files without external pointers, links or alternates",
    )
}

/// Owns the only filesystem authority passed to the Git subprocess.
pub(super) struct Snapshot {
    path: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl Snapshot {
    pub(super) fn open(source: &Path) -> io::Result<Self> {
        let source_root = Root::open(source)?;
        let (worktree, common) = metadata(source, &source_root)?;
        if common
            .metadata_entries(MAX_ENTRIES)?
            .iter()
            .any(|(name, _)| name == "reftable")
        {
            return Err(invalid(
                "Git reftable storage is unsupported; use self-contained files-based refs",
            ));
        }
        let format = object_format(&common)?;
        let temporary = fs::canonicalize(std::env::temp_dir())?;
        // Initial aliases are resolved once. All copied metadata and command cwd are then
        // protected by this newly created, unpredictable, private directory.
        let path = temporary.join(format!("firecrab-git-{}", Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let snapshot = Self { path };
        let mut budget = Budget {
            bytes: 0,
            entries: 2,
        };
        fs::create_dir(snapshot.path.join("objects"))?;
        fs::create_dir(snapshot.path.join("refs"))?;
        let head = worktree
            .as_ref()
            .unwrap_or(&common)
            .metadata_read("HEAD", 4096)?;
        validate_ref(&head, format)?;
        snapshot.write("HEAD", &head, &mut budget)?;
        if let Some(refs) = optional_directory(&common, "refs")? {
            snapshot.copy_refs(&refs, "refs", 0, format, &mut budget)?;
        }
        if let Some(bytes) = optional_read(&common, "packed-refs", 4 * 1024 * 1024)? {
            validate_packed(&bytes, format)?;
            snapshot.write("packed-refs", &bytes, &mut budget)?;
        }
        if let Some(bytes) = optional_read(&common, "shallow", 1024 * 1024)? {
            let text = std::str::from_utf8(&bytes).map_err(|_| unsafe_metadata())?;
            if !text.lines().all(|line| oid(line.trim(), format)) {
                return Err(unsafe_metadata());
            }
            snapshot.write("shallow", &bytes, &mut budget)?;
        }
        let objects = common.metadata_directory("objects")?;
        if let Some(info) = optional_directory(&objects, "info")?
            && info
                .metadata_entries(MAX_ENTRIES)?
                .iter()
                .any(|(name, _)| matches!(name.as_str(), "alternates" | "http-alternates"))
        {
            return Err(unsafe_metadata());
        }
        for (name, kind) in objects.metadata_entries(MAX_ENTRIES)? {
            budget.entry()?;
            if kind.is_symlink() {
                return Err(unsafe_metadata());
            }
            if name == "info" {
                continue;
            }
            if !kind.is_dir() {
                return Err(unsafe_metadata());
            }
            let child = objects.metadata_directory(&name)?;
            if name == "pack" {
                for (pack, kind) in child.metadata_entries(MAX_ENTRIES)? {
                    if !kind.is_file() {
                        return Err(unsafe_metadata());
                    }
                    if pack == "multi-pack-index"
                        || ["bitmap", "keep", "promisor"].iter().any(|extension| {
                            pack.strip_suffix(&format!(".{extension}"))
                                .is_some_and(|name| {
                                    name.strip_prefix("pack-")
                                        .is_some_and(|name| oid(name, format))
                                })
                        })
                    {
                        continue;
                    }
                    if !pack_name(&pack, format) {
                        return Err(unsafe_metadata());
                    }
                    let bytes = child.metadata_read(&pack, MAX_BYTES - budget.bytes)?;
                    snapshot.write(&format!("objects/pack/{pack}"), &bytes, &mut budget)?;
                }
            } else {
                if name.len() != 2 || !hex(&name) {
                    return Err(unsafe_metadata());
                }
                for (object, kind) in child.metadata_entries(MAX_ENTRIES)? {
                    if !kind.is_file() || object.len() != format - 2 || !hex(&object) {
                        return Err(unsafe_metadata());
                    }
                    let bytes = child.metadata_read(&object, MAX_BYTES - budget.bytes)?;
                    snapshot.write(&format!("objects/{name}/{object}"), &bytes, &mut budget)?;
                }
            }
        }
        let config = if format == 64 {
            "[core]\nrepositoryformatversion=1\nbare=true\nhooksPath=/dev/null\nfsmonitor=false\n[extensions]\nobjectFormat=sha256\n"
        } else {
            "[core]\nrepositoryformatversion=0\nbare=true\nhooksPath=/dev/null\nfsmonitor=false\n"
        };
        snapshot.write("config", config.as_bytes(), &mut budget)?;
        Ok(snapshot)
    }

    fn write(&self, path: &str, bytes: &[u8], budget: &mut Budget) -> io::Result<()> {
        budget.entry()?;
        budget.bytes = budget
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(unsafe_metadata)?;
        if budget.bytes > MAX_BYTES {
            return Err(invalid(
                "Git metadata exceeds 128 MiB; use a smaller self-contained repository",
            ));
        }
        let path = self.path.join(path);
        fs::create_dir_all(path.parent().ok_or_else(unsafe_metadata)?)?;
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?
            .write_all(bytes)
    }

    fn copy_refs(
        &self,
        source: &Root,
        prefix: &str,
        depth: usize,
        format: usize,
        budget: &mut Budget,
    ) -> io::Result<()> {
        if depth > 32 {
            return Err(unsafe_metadata());
        }
        for (name, kind) in source.metadata_entries(MAX_ENTRIES)? {
            let path = format!("{prefix}/{name}");
            if !ref_name(&path) {
                return Err(unsafe_metadata());
            }
            if kind.is_dir() {
                budget.entry()?;
                self.copy_refs(
                    &source.metadata_directory(&name)?,
                    &path,
                    depth + 1,
                    format,
                    budget,
                )?;
            } else if kind.is_file() {
                let bytes = source.metadata_read(&name, 4096)?;
                validate_ref(&bytes, format)?;
                self.write(&path, &bytes, budget)?;
            } else {
                return Err(unsafe_metadata());
            }
        }
        Ok(())
    }

    /// No original Git/config/objects path is given to this subprocess.
    pub(super) fn run(&self, args: &[&str], limit: usize) -> io::Result<Vec<u8>> {
        let directory = Root::open(&self.path)?;
        let mut command = Command::new("/usr/bin/git");
        command
            .current_dir(directory.command_directory())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .arg("--git-dir=.")
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the post-fork hook only applies fixed process limits via libc.
            unsafe {
                command.pre_exec(|| {
                    for (resource, value) in [
                        (libc::RLIMIT_AS, 512 * 1024 * 1024),
                        (libc::RLIMIT_CPU, 15),
                        (libc::RLIMIT_NOFILE, 64),
                    ] {
                        let limit = libc::rlimit {
                            rlim_cur: value,
                            rlim_max: value,
                        };
                        if libc::setrlimit(resource, &limit) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
        let mut child = command
            .spawn()
            .map_err(|_| invalid("Cannot run /usr/bin/git"))?;
        let mut bytes = Vec::new();
        let read = child
            .stdout
            .take()
            .ok_or_else(|| invalid("Missing Git output"))?
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes);
        if read.is_err() || bytes.len() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("Git source exceeds the context or snapshot limit"));
        }
        if !child.wait()?.success() {
            return Err(invalid(
                "Cannot read the selected local Git ref from its bounded metadata snapshot",
            ));
        }
        Ok(bytes)
    }
}

struct Budget {
    bytes: usize,
    entries: usize,
}
impl Budget {
    fn entry(&mut self) -> io::Result<()> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(invalid(
                "Git metadata exceeds 32768 entries; use a smaller self-contained repository",
            ));
        }
        Ok(())
    }
}

fn metadata(source: &Path, source_root: &Root) -> io::Result<(Option<Root>, Root)> {
    let directory = match source_root.metadata_directory(".git") {
        Ok(directory) => Some(directory),
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => None,
        Err(error) => return Err(error),
    };
    if let Some(directory) = directory {
        if optional_read(&directory, "commondir", 4096)?.is_some() {
            return Err(unsafe_metadata());
        }
        return Ok((None, directory));
    }
    let marker = source_root.metadata_read(".git", 4096)?;
    let marker = std::str::from_utf8(&marker)
        .map_err(|_| unsafe_metadata())?
        .trim();
    let directory = absolute(
        source,
        marker
            .strip_prefix("gitdir: ")
            .ok_or_else(unsafe_metadata)?,
    )?;
    if directory.parent().and_then(Path::file_name) != Some(std::ffi::OsStr::new("worktrees")) {
        return Err(unsafe_metadata());
    }
    let expected_common = directory
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unsafe_metadata)?;
    let common = Root::open(expected_common)?;
    let worktree = Root::open(&directory)?;
    if common.metadata_owner()? != source_root.metadata_owner()?
        || worktree.metadata_owner()? != source_root.metadata_owner()?
    {
        return Err(unsafe_metadata());
    }
    let common_text = worktree.metadata_read("commondir", 4096)?;
    let common_text = std::str::from_utf8(&common_text)
        .map_err(|_| unsafe_metadata())?
        .trim();
    let common_path = absolute(&directory, common_text)?;
    if expected_common != common_path {
        return Err(unsafe_metadata());
    }
    let backlink = worktree.metadata_read("gitdir", 4096)?;
    let backlink = std::str::from_utf8(&backlink)
        .map_err(|_| unsafe_metadata())?
        .trim();
    if absolute(&directory, backlink)? != source.join(".git") {
        return Err(unsafe_metadata());
    }
    Ok((Some(worktree), common))
}

fn optional_directory(source: &Root, path: &str) -> io::Result<Option<Root>> {
    match source.metadata_directory(path) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn optional_read(source: &Root, path: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
    match source.metadata_read(path, limit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
fn absolute(base: &Path, text: &str) -> io::Result<PathBuf> {
    if text.is_empty()
        || text.len() > 4096
        || text.contains('\\')
        || text.chars().any(char::is_control)
    {
        return Err(unsafe_metadata());
    }
    let input = if Path::new(text).is_absolute() {
        PathBuf::from(text)
    } else {
        base.join(text)
    };
    let mut result = PathBuf::new();
    for part in input.components() {
        match part {
            Component::RootDir => result.push("/"),
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return Err(unsafe_metadata()),
        }
    }
    if !result.is_absolute() {
        return Err(unsafe_metadata());
    }
    Ok(result)
}
fn hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn oid(value: &str, format: usize) -> bool {
    value.len() == format && hex(value)
}
fn ref_name(value: &str) -> bool {
    value.starts_with("refs/")
        && value.len() <= 1000
        && !value.contains("..")
        && !value.contains("@{")
        && value.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
        })
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
        && value.bytes().all(|b| !b"~^:?*[\\".contains(&b))
}
fn validate_ref(bytes: &[u8], format: usize) -> io::Result<()> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| unsafe_metadata())?
        .trim();
    if oid(text, format) || text.strip_prefix("ref: ").is_some_and(ref_name) {
        Ok(())
    } else {
        Err(unsafe_metadata())
    }
}
fn validate_packed(bytes: &[u8], format: usize) -> io::Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| unsafe_metadata())?;
    for line in text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        if let Some(peeled) = line.strip_prefix('^')
            && oid(peeled, format)
        {
            continue;
        }
        let Some((id, name)) = line.split_once(' ') else {
            return Err(unsafe_metadata());
        };
        if !oid(id, format) || !ref_name(name) {
            return Err(unsafe_metadata());
        }
    }
    Ok(())
}
fn pack_name(name: &str, format: usize) -> bool {
    let Some((name, extension)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "pack" | "idx" | "rev")
        && name
            .strip_prefix("pack-")
            .is_some_and(|name| oid(name, format))
}
fn object_format(common: &Root) -> io::Result<usize> {
    let mut format = 40;
    if let Some(config) = optional_read(common, "config", 64 * 1024)? {
        let text = std::str::from_utf8(&config).map_err(|_| unsafe_metadata())?;
        let mut extensions = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                extensions = line.eq_ignore_ascii_case("[extensions]");
                continue;
            }
            if !extensions {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            if key.trim().eq_ignore_ascii_case("objectformat") {
                format = match value {
                    "sha1" => 40,
                    "sha256" => 64,
                    _ => return Err(unsafe_metadata()),
                };
            }
            if key.trim().eq_ignore_ascii_case("refstorage") && value != "files" {
                return Err(invalid(
                    "Git reftable storage is unsupported; use self-contained files-based refs",
                ));
            }
            if key.trim().eq_ignore_ascii_case("partialclone") {
                return Err(invalid(
                    "Git partial clones are unsupported; use self-contained objects",
                ));
            }
        }
    }
    Ok(format)
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
