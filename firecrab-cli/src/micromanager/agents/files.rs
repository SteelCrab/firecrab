//! Read-only listing of one project folder in the MicroVM, for the task page's folder tree.
use super::*;

/// Entries shown for one folder. A larger folder is marked truncated.
const LIMIT: usize = 500;
/// Room for LIMIT records of the longest Linux file names.
const CAPACITY: u64 = 160 * 1024;

#[derive(Serialize)]
struct Entry {
    name: String,
    kind: &'static str,
}

/// A folder path relative to the project root. "" is the root itself.
fn relative(path: &str) -> Result<&str, Error> {
    if path.is_empty() {
        return Ok(path);
    }
    if path.len() > 1024
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || part.len() > 255 || [".", "..", ".git"].contains(&part))
    {
        return Err(invalid("Choose a folder inside the project."));
    }
    Ok(path)
}

/// Lists one folder by name and type. The shell enters the folder first and then
/// checks that its physical path is the requested one, so a symlink anywhere in
/// the path (inside or outside the project) is rejected instead of followed.
fn listing_script(root: &str, path: &str) -> String {
    let suffix = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    format!(
        r#"set -eu
cd -P -- {root} 2>/dev/null || {{ printf '%s\n' 'The project folder is missing in this MicroVM.' >&2; exit 1; }}
want="$(pwd -P)"{suffix}
cd -P -- "$want" 2>/dev/null || {{ printf '%s\n' 'This folder no longer exists.' >&2; exit 1; }}
[ "$(pwd -P)" = "$want" ] || {{ printf '%s\n' 'This folder is outside the project.' >&2; exit 1; }}
set -- * .[!.]* ..?*
count=0
# Folders go out first, so a capped listing never hides them behind files.
for pass in d f; do
  for name in "$@"; do
    [ "$name" != .git ] || continue
    if [ -L "$name" ]; then kind=l; elif [ -d "$name" ]; then kind=d; elif [ -f "$name" ]; then kind=f; elif [ -e "$name" ]; then kind=o; else continue; fi
    case $pass$kind in dd | f[!d]) ;; *) continue ;; esac
    if [ "$count" -ge {LIMIT} ]; then printf 'more\000'; break 2; fi
    count=$((count + 1))
    printf '%s\t%s\000' "$kind" "$name"
  done
done
"#,
        root = guest::quote(root),
        suffix = guest::quote(&suffix),
    )
}

/// NUL-separated `kind<TAB>name` records, folders first and then by name.
fn parse_listing(output: &str) -> (Vec<Entry>, bool) {
    let mut records = output.split('\0').collect::<Vec<_>>();
    // Complete output ends with NUL. Anything after the last one was cut off.
    let mut truncated = records.pop().is_some_and(|tail| !tail.is_empty());
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for record in records {
        if record == "more" {
            truncated = true;
            continue;
        }
        let Some((kind, name)) = record.split_once('\t') else {
            continue;
        };
        let kind = match kind {
            "d" => "directory",
            "f" => "file",
            "l" => "symlink",
            "o" => "other",
            _ => continue,
        };
        if name.is_empty()
            || name.contains('/')
            || [".", "..", ".git"].contains(&name)
            || !seen.insert(name)
        {
            continue;
        }
        entries.push(Entry {
            name: name.into(),
            kind,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != "directory")
            .cmp(&(b.kind != "directory"))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    if entries.len() > LIMIT {
        entries.truncate(LIMIT);
        truncated = true;
    }
    (entries, truncated)
}

impl Runtime {
    /// One folder of the VM project. Read-only; `.git` is never listed.
    pub fn project_files(&self, path: &str) -> Result<Value, Error> {
        let path = relative(path)?;
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let project = self.vm_project()?;
        let root = match (project["state"].as_str(), project["path"].as_str()) {
            (Some("ready"), Some(root)) => root,
            _ => {
                return Err(invalid("Connect a repository before browsing its files."));
            }
        };
        let output = guest.shell_capture(
            &listing_script(root, path),
            Duration::from_secs(15),
            CAPACITY,
        )?;
        let (entries, truncated) = parse_listing(&output);
        Ok(json!({"path": path, "entries": entries, "truncated": truncated}))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{guest_fixture, shell_replies};
    use super::*;

    fn names(entries: &[Entry]) -> Vec<(&str, &str)> {
        entries.iter().map(|e| (e.name.as_str(), e.kind)).collect()
    }

    #[test]
    fn only_plain_folders_inside_the_project_are_accepted() {
        for path in ["", "src", "src/components", ".github", "a b/it's", "한글"] {
            assert_eq!(relative(path).unwrap(), path);
        }
        let long = "a/".repeat(600);
        for path in [
            "/etc",
            "/",
            "..",
            "../etc",
            "src/../..",
            "src/..",
            ".",
            "./src",
            "src/",
            "src//lib",
            ".git",
            "src/.git/hooks",
            "a\nb",
            "a\0b",
            long.trim_end_matches('/'),
        ] {
            assert!(relative(path).is_err(), "{path:?} must be rejected");
        }
        assert!(relative(&"x".repeat(256)).is_err());
    }

    #[test]
    fn listings_put_folders_first_and_drop_git_and_damaged_records() {
        let output = "f\tzeta.txt\0d\tsrc\0f\tAlpha.md\0d\t.github\0l\tlink\0d\t.git\0\
                      x\tunknown\0d\ta/b\0d\t..\0f\t\0garbage\0o\tsocket\0f\tbeta\0";
        let (entries, truncated) = parse_listing(output);
        assert!(!truncated);
        assert_eq!(
            names(&entries),
            [
                (".github", "directory"),
                ("src", "directory"),
                ("Alpha.md", "file"),
                ("beta", "file"),
                ("link", "symlink"),
                ("socket", "other"),
                ("zeta.txt", "file"),
            ]
        );
    }

    #[test]
    fn large_or_cut_listings_are_marked_truncated() {
        let (entries, truncated) = parse_listing("f\ta\0more\0");
        assert_eq!(names(&entries), [("a", "file")]);
        assert!(truncated);
        // Output cut by the capture limit ends inside a record.
        let (entries, truncated) = parse_listing("f\ta\0f\tpart");
        assert_eq!(names(&entries), [("a", "file")]);
        assert!(truncated);
        let many = (0..LIMIT + 5)
            .map(|i| format!("f\tfile-{i:04}\0"))
            .collect::<String>();
        let (entries, truncated) = parse_listing(&many);
        assert_eq!(entries.len(), LIMIT);
        assert!(truncated);
    }

    #[test]
    fn a_name_listed_twice_keeps_only_its_first_record() {
        // The guest lists folders in a first pass, so a folder replaced by a
        // file between the passes can appear twice.
        let (entries, _) = parse_listing("d\tsrc\0f\tREADME.md\0f\tsrc\0");
        assert_eq!(
            names(&entries),
            [("src", "directory"), ("README.md", "file")]
        );
    }

    #[cfg(unix)]
    fn list(shell: &str, root: &Path, path: &str) -> Result<(Vec<Entry>, bool), String> {
        let output = Command::new(shell)
            .arg("-c")
            .arg(listing_script(&root.to_string_lossy(), path))
            .output()
            .unwrap();
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        Ok(parse_listing(&String::from_utf8_lossy(&output.stdout)))
    }

    #[cfg(unix)]
    #[test]
    fn the_guest_script_lists_one_folder_without_leaving_the_project() {
        // Debian guests run dash as /bin/sh; check it too when the host has it.
        for shell in ["/bin/sh", "/bin/dash"] {
            if Path::new(shell).exists() {
                guest_script_stays_in_the_project(shell);
            }
        }
    }

    #[cfg(unix)]
    fn guest_script_stays_in_the_project(shell: &str) {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        let root = temp.path().join("project");
        fs::create_dir_all(outside.join("secret")).unwrap();
        fs::write(outside.join("secret/key"), "x").unwrap();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::write(root.join("README.md"), "").unwrap();
        fs::write(root.join(".env"), "").unwrap();
        fs::write(root.join("-n"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        symlink("src", root.join("inside")).unwrap();
        symlink("missing", root.join("broken")).unwrap();

        let (entries, truncated) = list(shell, &root, "").unwrap();
        assert!(!truncated);
        assert_eq!(
            names(&entries),
            [
                (".github", "directory"),
                ("src", "directory"),
                ("-n", "file"),
                (".env", "file"),
                ("broken", "symlink"),
                ("escape", "symlink"),
                ("inside", "symlink"),
                ("README.md", "file"),
            ]
        );
        let (entries, _) = list(shell, &root, "src").unwrap();
        assert_eq!(
            names(&entries),
            [("nested", "directory"), ("main.rs", "file")]
        );
        assert!(list(shell, &root, "src/nested").unwrap().0.is_empty());
        // Symlinks are never followed, even to a folder inside the project.
        assert!(list(shell, &root, "escape").is_err());
        assert!(list(shell, &root, "escape/secret").is_err());
        assert!(list(shell, &root, "inside").is_err());
        assert!(list(shell, &root, "missing").is_err());
        assert!(list(shell, &root, "README.md").is_err());

        let big = root.join("big");
        fs::create_dir(&big).unwrap();
        for i in 0..LIMIT + 3 {
            fs::write(big.join(format!("f{i}")), "").unwrap();
        }
        let (entries, truncated) = list(shell, &root, "big").unwrap();
        assert_eq!(entries.len(), LIMIT);
        assert!(truncated);
    }

    #[cfg(unix)]
    #[test]
    fn a_long_listing_keeps_folders_before_the_cap() {
        for shell in ["/bin/sh", "/bin/dash"] {
            if !Path::new(shell).exists() {
                continue;
            }
            let temp = tempfile::tempdir().unwrap();
            for i in 0..LIMIT + 3 {
                fs::write(temp.path().join(format!("a{i:04}")), "").unwrap();
            }
            // Both names sort after every file, so a cap applied in name order drops them.
            fs::create_dir(temp.path().join("zz-folder")).unwrap();
            fs::create_dir(temp.path().join(".zz-hidden")).unwrap();
            let (entries, truncated) = list(shell, temp.path(), "").unwrap();
            assert!(truncated, "{shell}");
            assert_eq!(entries.len(), LIMIT, "{shell}");
            assert_eq!(
                names(&entries[..2]),
                [(".zz-hidden", "directory"), ("zz-folder", "directory")],
                "{shell}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn project_files_need_a_ready_project_and_reject_paths_before_the_guest() {
        let (root, runtime) = guest_fixture(true);
        assert!(runtime.project_files("").is_err());
        let manifest = project::VmProject {
            vm_id: runtime.guest.as_ref().unwrap().id,
            repository: Some("https://github.com/example/project.git".into()),
            branch: "main".into(),
            path: project::WORKSPACE.into(),
        };
        fs::create_dir_all(&runtime.home).unwrap();
        fs::write(
            root.path().join("guest-project.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        shell_replies(
            root.path(),
            json!({"pwd -P": {"out": "f\tREADME.md\0d\tsrc\0"}}),
        );
        let value = runtime.project_files("src").unwrap();
        assert_eq!(value["path"], "src");
        assert_eq!(value["truncated"], false);
        assert_eq!(
            value["entries"],
            json!([{"name":"src","kind":"directory"},{"name":"README.md","kind":"file"}])
        );
        let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
        let listing = calls.lines().filter(|l| l.contains("pwd -P")).count();
        assert_eq!(listing, 1);
        assert!(calls.contains("/workspace/firecrab/project"));
        for path in ["../etc", "/etc", ".git"] {
            assert!(runtime.project_files(path).is_err());
        }
        let calls = fs::read_to_string(root.path().join("remote-calls.jsonl")).unwrap();
        assert_eq!(calls.lines().filter(|l| l.contains("pwd -P")).count(), 1);
    }
}
