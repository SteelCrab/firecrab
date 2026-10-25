//! Task-owned project copies, bounded archives and explicit conflict-checked source updates.
use super::files::{self, Root};
use firecrab_api_types::SandboxFileChange;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
};
use uuid::Uuid;
#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;

/// Stage a retained copy and a dedicated Git branch without changing the source repository.
pub fn prepare(
    source: &Path,
    attempt: &Path,
    id: Uuid,
    database: Option<&Path>,
    credentials: &[String],
) -> io::Result<()> {
    fs::create_dir_all(attempt)?;
    fs::set_permissions(attempt, fs::Permissions::from_mode(0o700))?;
    for name in ["base", "work"] {
        fs::create_dir(attempt.join(name))?;
    }
    let excluded = database
        .map(|path| {
            let mut paths = vec![path.to_path_buf()];
            for suffix in ["-wal", "-shm", "-journal"] {
                paths.push(std::path::PathBuf::from(format!(
                    "{}{suffix}",
                    path.display()
                )));
            }
            paths
        })
        .unwrap_or_default();
    files::snapshot_excluding(source, &attempt.join("base"), &excluded, credentials)?;
    let root = Root::open(source)?;
    let modes: BTreeMap<_, _> = root
        .files()?
        .into_iter()
        .map(|p| root.mode(&p).map(|mode| (p, mode)))
        .collect::<io::Result<_>>()?;
    fs::write(attempt.join("base-modes.json"), serde_json::to_vec(&modes)?)?;
    files::snapshot(&attempt.join("base"), &attempt.join("work"))?;
    let work = attempt.join("work");
    for args in [
        vec![
            "init".to_owned(),
            "-q".into(),
            "-b".into(),
            format!("ai/{id}"),
        ],
        vec!["add".into(), "-f".into(), ".".into()],
        vec![
            "commit".into(),
            "-q".into(),
            "--no-gpg-sign".into(),
            "--allow-empty".into(),
            "-m".into(),
            "Sandbox project snapshot".into(),
        ],
    ] {
        let result = Command::new("/usr/bin/git")
            .current_dir(&work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "user.name=Firecrab Sandbox",
                "-c",
                "user.email=sandbox@localhost",
            ])
            .args(args)
            .output()
            .map_err(|_| {
                io::Error::other("Cannot run /usr/bin/git; install Git on the API host")
            })?;
        if !result.status.success() {
            return Err(io::Error::other(
                "Cannot initialize the isolated task branch",
            ));
        }
    }
    Ok(())
}

/// Review text changes against the retained initial snapshot, including newly created files.
pub fn changes(attempt: &Path) -> io::Result<Vec<SandboxFileChange>> {
    let before = Root::open(&attempt.join("base"))?;
    let after = Root::open(&attempt.join("work"))?;
    let modes: BTreeMap<String, u32> =
        serde_json::from_slice(&fs::read(attempt.join("base-modes.json"))?)?;
    let names: BTreeSet<_> = before.files()?.into_iter().chain(after.files()?).collect();
    let mut changes = Vec::new();
    let mut total = 0;
    for path in names {
        let a = optional(&before, &path)?;
        let b = optional(&after, &path)?;
        let before_mode = if a.is_some() {
            modes.get(&path).copied()
        } else {
            None
        };
        let after_mode = if b.is_some() {
            Some((before_mode.unwrap_or(0o644) & 0o666) | (after.mode(&path)? & 0o111))
        } else {
            None
        };
        if a == b && before_mode == after_mode {
            continue;
        }
        total += a.as_ref().map_or(0, Vec::len) + b.as_ref().map_or(0, Vec::len);
        if total > 2 * files::TREE_LIMIT {
            return Err(io::Error::other("Changes exceed the review limit"));
        }
        let a=a.map(String::from_utf8).transpose().map_err(|_|io::Error::other("Binary changes remain in the retained workspace; automatic apply supports text files only"))?;
        let b=b.map(String::from_utf8).transpose().map_err(|_|io::Error::other("Binary changes remain in the retained workspace; automatic apply supports text files only"))?;
        changes.push(SandboxFileChange {
            path,
            before: a,
            after: b,
            before_mode,
            after_mode,
            applied: false,
        });
    }
    Ok(changes)
}
/// Distinguish a missing regular file from an unsafe or unreadable path.
fn optional(root: &Root, path: &str) -> io::Result<Option<Vec<u8>>> {
    match root.read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Pack only bounded allowed files; never include Git metadata or credentials.
pub fn archive(work: &Path) -> io::Result<Vec<u8>> {
    let root = Root::open(work)?;
    let mut archive = tar::Builder::new(Vec::new());
    let mut total = 0;
    for path in root.files()? {
        let bytes = root.read(&path)?;
        total += bytes.len();
        if total > files::TREE_LIMIT {
            return Err(io::Error::other("Project archive exceeds 32 MiB"));
        }
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600 | (root.mode(&path)? & 0o111));
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive.append_data(&mut header, path, bytes.as_slice())?;
    }
    archive.into_inner()
}

/// Guest archives cannot create symlinks, devices, traversal paths or private host files.
pub fn import(work: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > files::TREE_LIMIT + 4 * 1024 * 1024 {
        return Err(io::Error::other("Guest archive exceeds the output limit"));
    }
    let staging = work.with_file_name(format!("sync-{}", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    let result = (|| {
        let root = Root::open(&staging)?;
        let mut total = 0;
        let mut count = 0;
        for entry in tar::Archive::new(bytes).entries()? {
            let mut entry = entry?;
            let name = entry
                .path()?
                .to_str()
                .ok_or_else(|| io::Error::other("Invalid guest path"))?
                .trim_start_matches("./")
                .to_owned();
            if name.is_empty() || entry.header().entry_type().is_dir() {
                continue;
            }
            if !files::allowed(&name) {
                continue;
            }
            if !entry.header().entry_type().is_file() {
                return Err(io::Error::other(
                    "Guest archive contains a link or special file",
                ));
            }
            let mut data = Vec::new();
            entry
                .by_ref()
                .take((files::FILE_LIMIT + 1) as u64)
                .read_to_end(&mut data)?;
            total += data.len();
            count += 1;
            if total > files::TREE_LIMIT || count > files::FILE_COUNT {
                return Err(io::Error::other("Guest workspace exceeds limits"));
            }
            root.write(&name, &data)?;
            root.set_mode(&name, 0o600 | (entry.header().mode()? & 0o111))?;
        }
        let old = Root::open(work)?;
        let new = Root::open(&staging)?;
        for path in old.files()? {
            if optional(&new, &path)?.is_none() {
                old.remove(&path)?;
            }
        }
        files::snapshot(&staging, work)
    })();
    let _ = fs::remove_dir_all(&staging);
    result
}

/// Verify every selected base before writing. Roll back previous writes if an I/O operation fails.
pub fn apply(source: &Path, changes: &mut [SandboxFileChange], paths: &[String]) -> io::Result<()> {
    if paths.is_empty() {
        return Err(io::Error::other("Select at least one file"));
    }
    let root = Root::open(source)?;
    let mut selected = Vec::new();
    for path in paths {
        if selected.iter().any(|i: &usize| changes[*i].path == *path) {
            return Err(io::Error::other("Duplicate review path"));
        }
        let index = changes
            .iter()
            .position(|c| c.path == *path && !c.applied)
            .ok_or_else(|| io::Error::other("Unknown or already applied change"))?;
        if optional(&root, path)?
            != changes[index]
                .before
                .as_ref()
                .map(|s| s.as_bytes().to_vec())
        {
            return Err(io::Error::other(
                "Source changed since the task snapshot. Review the conflict before applying.",
            ));
        }
        let actual_mode = match root.mode(path) {
            Ok(mode) => Some(mode),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if actual_mode != changes[index].before_mode {
            return Err(io::Error::other(
                "Source permissions changed since the task snapshot",
            ));
        }
        selected.push(index);
    }
    let mut written = Vec::new();
    for &index in &selected {
        let change = &changes[index];
        let result = match &change.after {
            Some(text) => root
                .write(&change.path, text.as_bytes())
                .and_then(|_| root.set_mode(&change.path, change.after_mode.unwrap_or(0o644))),
            None => root.remove(&change.path),
        };
        if let Err(error) = result {
            for &index in written.iter().rev() {
                let change: &SandboxFileChange = &changes[index];
                match &change.before {
                    Some(text) => {
                        let _ = root.write(&change.path, text.as_bytes());
                        let _ = root.set_mode(&change.path, change.before_mode.unwrap_or(0o644));
                    }
                    None => {
                        let _ = root.remove(&change.path);
                    }
                }
            }
            return Err(error);
        }
        written.push(index);
    }
    for index in selected {
        changes[index].applied = true;
    }
    Ok(())
}
