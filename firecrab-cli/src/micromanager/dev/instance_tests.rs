use super::*;

struct Repository {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    main: PathBuf,
    first: PathBuf,
    second: PathBuf,
    primary: PathBuf,
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(git_path(directory).as_ref())
        .args(
            args.iter()
                .map(|argument| git_path(Path::new(argument)).into_owned()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

impl Repository {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let main = root.join("main");
        let first = root.join("first worktree 한글");
        let second = root.join("second worktree");
        let primary = root.join("managed/micromanager");
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(primary.join("runtime")).unwrap();
        fs::write(primary.join("runtime/dev-dashboard.json"), "primary state").unwrap();
        git(&main, &["init", "--initial-branch=main"]);
        fs::write(main.join("README.md"), "fixture\n").unwrap();
        git(&main, &["add", "README.md"]);
        let hooks = root.join("empty-hooks");
        fs::create_dir(&hooks).unwrap();
        git(
            &main,
            &[
                "-c",
                "user.name=Development fixture",
                "-c",
                "user.email=development@example.invalid",
                "-c",
                &format!("core.hooksPath={}", git_path(&hooks).display()),
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "fixture",
            ],
        );
        git(
            &main,
            &["worktree", "add", "-b", "first", first.to_str().unwrap()],
        );
        git(
            &main,
            &["worktree", "add", "-b", "second", second.to_str().unwrap()],
        );
        Self {
            _temporary: temporary,
            root,
            main,
            first,
            second,
            primary,
        }
    }

    fn instance(&self, source: &Path, port: u16) -> Instance {
        resolve(source, &self.primary, true, port).unwrap().unwrap()
    }

    fn primary_is_preserved(&self) {
        assert_eq!(
            fs::read_to_string(self.primary.join("runtime/dev-dashboard.json")).unwrap(),
            "primary state"
        );
        assert!(load(&self.primary).unwrap().is_none());
    }
}

#[cfg(windows)]
#[test]
fn git_arguments_preserve_unicode_while_removing_only_verbatim_drive_and_unc_prefixes() {
    for (canonical, expected) in [
        (
            r"\\?\C:\worktrees\first worktree 한글",
            r"C:\worktrees\first worktree 한글",
        ),
        (
            r"\\?\UNC\server\share\worktree 한글",
            r"\\server\share\worktree 한글",
        ),
        (r"C:\worktrees\first", r"C:\worktrees\first"),
        (r"\\server\share\first", r"\\server\share\first"),
        (r"relative\first", r"relative\first"),
    ] {
        assert_eq!(git_path(Path::new(canonical)).as_ref(), Path::new(expected));
    }
}

#[test]
fn the_primary_checkout_never_creates_a_private_instance() {
    let repository = Repository::new();
    for create in [false, true] {
        assert!(
            resolve(&repository.main, &repository.primary, create, 8080)
                .unwrap()
                .is_none()
        );
    }
    assert!(!repository.root.join("managed/dev-instances").exists());
    repository.primary_is_preserved();
}

#[test]
fn linked_worktrees_get_separate_homes_labels_and_backend_ports() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let second = repository.instance(&repository.second, 8099);
    assert_ne!(first.id, second.id);
    assert_ne!(first.git_dir, second.git_dir);
    assert_ne!(first.managed_home, second.managed_home);
    assert_ne!(first.install_dir, second.install_dir);
    assert_ne!(first.managed_home, repository.primary);
    let ports = [
        first.api_port,
        first.resource_port,
        second.api_port,
        second.resource_port,
    ];
    assert_eq!(ports.into_iter().collect::<HashSet<_>>().len(), 4);
    assert!(ports.iter().all(|port| *port >= 1024));
    assert!(
        ports
            .iter()
            .all(|port| ![5523, 5524, 8080, 8098, 8099].contains(port))
    );
    assert_ne!(
        label(&first.managed_home, "io.firecrab.micromanager").unwrap(),
        label(&second.managed_home, "io.firecrab.micromanager").unwrap()
    );
    assert_eq!(api_port(&first.managed_home).unwrap(), first.api_port);
    assert_eq!(
        resource_port(&second.managed_home).unwrap(),
        second.resource_port
    );
    repository.primary_is_preserved();
}

#[test]
fn source_subdirectories_and_branch_switches_reuse_the_same_git_identity() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let nested = repository.first.join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    git(&repository.first, &["switch", "-c", "renamed-branch"]);
    let selected = resolve(&nested, &repository.primary, false, 8098)
        .unwrap()
        .unwrap();
    assert_eq!(selected, first);
    assert_eq!(
        selected.source,
        fs::canonicalize(&repository.first).unwrap()
    );
    repository.primary_is_preserved();
}

#[test]
fn repeated_resolution_reuses_the_record_and_allocated_ports() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let before = fs::read(first.managed_home.join(RECORD)).unwrap();
    for create in [true, false, true] {
        assert_eq!(
            resolve(&repository.first, &repository.primary, create, 8098)
                .unwrap()
                .unwrap(),
            first
        );
    }
    assert_eq!(fs::read(first.managed_home.join(RECORD)).unwrap(), before);
    repository.primary_is_preserved();
}

#[test]
fn existing_backend_ports_outside_the_new_pool_remain_unchanged() {
    let repository = Repository::new();
    let mut first = repository.instance(&repository.first, 8098);
    first.api_port = 60230;
    first.resource_port = 60231;
    let record = first.managed_home.join(RECORD);
    let before = serde_json::to_vec(&first).unwrap();
    fs::write(&record, &before).unwrap();
    for create in [false, true] {
        assert_eq!(
            resolve(&repository.first, &repository.primary, create, 8098)
                .unwrap()
                .unwrap(),
            first
        );
        assert_eq!(load(&first.managed_home).unwrap().unwrap(), first);
        assert_eq!(fs::read(&record).unwrap(), before);
    }
    repository.primary_is_preserved();
}

#[test]
fn primary_without_a_registry_reserves_defaults_without_creating_state() {
    let repository = Repository::new();
    let registry = repository.root.join("managed/dev-instances");
    assert_eq!(
        registered_backend_ports(&repository.primary).unwrap(),
        HashSet::from([5523, 5524, 8080])
    );
    assert!(!registry.exists());
    repository.primary_is_preserved();
}

#[test]
fn primary_and_private_reservations_include_later_registered_worktrees() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let before = HashSet::from([5523, 5524, 8080, first.api_port, first.resource_port]);
    for home in [&repository.primary, &first.managed_home] {
        assert_eq!(registered_backend_ports(home).unwrap(), before);
    }
    let second = repository.instance(&repository.second, 8099);
    let mut after = before;
    after.extend([second.api_port, second.resource_port]);
    for home in [
        &repository.primary,
        &first.managed_home,
        &second.managed_home,
    ] {
        assert_eq!(registered_backend_ports(home).unwrap(), after);
    }
    repository.primary_is_preserved();
}

#[test]
fn reservation_reads_wait_until_a_new_ownership_record_is_complete() {
    use std::sync::mpsc;
    use std::time::Duration;

    let repository = Repository::new();
    repository.instance(&repository.first, 8098);
    let directory = repository.root.join("managed");
    let registry = directory.join("dev-instances");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(registry.join("registry.lock"))
        .unwrap();
    FileExt::lock_exclusive(&lock).unwrap();
    fs::create_dir_all(registry.join("0123456789abcdef/micromanager")).unwrap();
    let primary = repository.primary.clone();
    let (started, ready) = mpsc::channel();
    let (sent, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        started.send(()).unwrap();
        sent.send(registered_backend_ports(&primary)).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        received.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let registered = test_instance(&directory, "0123456789abcdef", 60230, 60231);
    drop(lock);
    let ports = received
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert!(ports.contains(&registered.api_port));
    assert!(ports.contains(&registered.resource_port));
    reader.join().unwrap();
    repository.primary_is_preserved();
}

#[test]
fn removed_worktrees_do_not_block_primary_or_other_private_instances() {
    for use_git in [true, false] {
        let repository = Repository::new();
        let first = repository.instance(&repository.first, 8098);
        if use_git {
            git(
                &repository.main,
                &["worktree", "remove", repository.first.to_str().unwrap()],
            );
        } else {
            fs::remove_dir_all(&repository.first).unwrap();
            fs::remove_dir_all(&first.git_dir).unwrap();
        }
        assert!(!first.source.exists());
        assert!(!first.git_dir.exists());
        assert_eq!(load(&first.managed_home).unwrap().unwrap(), first);
        for create in [false, true] {
            assert!(
                resolve(&repository.main, &repository.primary, create, 8080)
                    .unwrap()
                    .is_none()
            );
            assert!(resolve(&repository.first, &repository.primary, create, 8098).is_err());
        }
        let second = repository.instance(&repository.second, 8099);
        assert_eq!(
            resolve(&repository.second, &repository.primary, false, 8099)
                .unwrap()
                .unwrap(),
            second
        );
        assert_eq!(api_port(&second.managed_home).unwrap(), second.api_port);
        assert_eq!(
            resource_port(&second.managed_home).unwrap(),
            second.resource_port
        );
        repository.primary_is_preserved();
    }
}

#[test]
fn missing_source_paths_still_require_absolute_normal_metadata() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let record = first.managed_home.join(RECORD);
    // PathBuf::push normalizes '..' when a Windows verbatim prefix is present.
    // Build the malformed metadata as raw text so the invalid component survives.
    let non_normal = |leaf: &str| {
        let mut path = repository.root.as_os_str().to_os_string();
        for component in ["main", "..", leaf] {
            path.push(std::path::MAIN_SEPARATOR_STR);
            path.push(component);
        }
        let path = PathBuf::from(path);
        assert!(
            path.components()
                .any(|part| matches!(part, Component::ParentDir))
        );
        path
    };
    for source in [
        PathBuf::from("missing-source"),
        non_normal("missing-source"),
    ] {
        let mut invalid = first.clone();
        invalid.source = source;
        fs::write(&record, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(load(&first.managed_home).is_err());
    }
    for git_dir in [
        PathBuf::from("missing-git-dir"),
        non_normal("missing-git-dir"),
    ] {
        let mut invalid = first.clone();
        invalid.git_dir = git_dir;
        fs::write(&record, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(load(&first.managed_home).is_err());
    }
    fs::write(record, serde_json::to_vec(&first).unwrap()).unwrap();
    repository.primary_is_preserved();
}

#[cfg(unix)]
#[test]
fn existing_symlinked_source_and_git_paths_remain_invalid() {
    use std::os::unix::fs::symlink;
    for replace_source in [true, false] {
        let repository = Repository::new();
        let first = repository.instance(&repository.first, 8098);
        let original = if replace_source {
            &first.source
        } else {
            &first.git_dir
        };
        let moved = repository.root.join("moved-path");
        fs::rename(original, &moved).unwrap();
        symlink(&moved, original).unwrap();
        assert!(load(&first.managed_home).is_err());
        fs::remove_dir_all(&moved).unwrap();
        assert!(load(&first.managed_home).is_err());
        repository.primary_is_preserved();
    }
}

#[test]
fn control_without_a_worktree_record_fails_instead_of_selecting_primary() {
    let repository = Repository::new();
    let error = resolve(&repository.first, &repository.primary, false, 8098).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!repository.root.join("managed/dev-instances").exists());
    repository.primary_is_preserved();
}

#[test]
fn broken_linked_git_metadata_never_selects_primary_from_a_nested_directory() {
    let repository = Repository::new();
    let nested = repository.first.join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        repository.first.join(".git"),
        "gitdir: /missing-private-worktree-metadata\n",
    )
    .unwrap();
    for source in [&repository.first, &nested] {
        assert!(resolve(source, &repository.primary, false, 8098).is_err());
    }
    repository.primary_is_preserved();
}

#[test]
fn deleting_registered_worktree_git_metadata_never_selects_primary() {
    let repository = Repository::new();
    repository.instance(&repository.first, 8098);
    let nested = repository.first.join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::remove_file(repository.first.join(".git")).unwrap();
    for source in [&repository.first, &nested] {
        for create in [false, true] {
            assert!(resolve(source, &repository.primary, create, 8098).is_err());
        }
    }
    repository.primary_is_preserved();
}

#[test]
fn deleting_nested_worktree_git_metadata_never_selects_its_parent_checkout() {
    let repository = Repository::new();
    let worktree = repository.main.join("nested-linked");
    git(
        &repository.main,
        &[
            "worktree",
            "add",
            "-b",
            "nested-linked",
            worktree.to_str().unwrap(),
        ],
    );
    repository.instance(&worktree, 8098);
    let nested = worktree.join("src/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::remove_file(worktree.join(".git")).unwrap();
    for source in [&worktree, &nested] {
        for create in [false, true] {
            assert!(resolve(source, &repository.primary, create, 8098).is_err());
        }
    }
    repository.primary_is_preserved();
}

#[test]
fn malformed_or_missing_private_metadata_never_falls_back_to_primary_ports() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let record = first.managed_home.join(RECORD);
    let valid = fs::read(&record).unwrap();
    fs::write(&record, "invalid private metadata").unwrap();
    assert!(load(&first.managed_home).is_err());
    assert!(api_port(&first.managed_home).is_err());
    assert!(resource_port(&first.managed_home).is_err());
    assert!(resolve(&repository.first, &repository.primary, false, 8098).is_err());
    fs::write(&record, valid).unwrap();
    fs::remove_file(&record).unwrap();
    assert!(load(&first.managed_home).is_err());
    assert!(api_port(&first.managed_home).is_err());
    assert!(resource_port(&first.managed_home).is_err());
    assert!(resolve(&repository.first, &repository.primary, true, 8098).is_err());
    repository.primary_is_preserved();
}

#[test]
fn conflicting_record_ports_block_all_private_instance_selection() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let second = repository.instance(&repository.second, 8099);
    let mut conflicting = second.clone();
    conflicting.api_port = first.api_port;
    fs::write(
        second.managed_home.join(RECORD),
        serde_json::to_vec(&conflicting).unwrap(),
    )
    .unwrap();
    for instance in [&first, &second] {
        assert!(load(&instance.managed_home).is_err());
        assert!(api_port(&instance.managed_home).is_err());
        assert!(resource_port(&instance.managed_home).is_err());
        assert!(registered_backend_ports(&instance.managed_home).is_err());
        for create in [false, true] {
            assert!(resolve(&instance.source, &repository.primary, create, 8098).is_err());
        }
    }
    assert!(registered_backend_ports(&repository.primary).is_err());
    repository.primary_is_preserved();
}

#[test]
fn records_copied_to_other_instances_or_primary_are_rejected() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let second = repository.instance(&repository.second, 8099);
    let bytes = fs::read(first.managed_home.join(RECORD)).unwrap();
    fs::write(second.managed_home.join(RECORD), &bytes).unwrap();
    assert!(load(&second.managed_home).is_err());
    assert!(resolve(&repository.second, &repository.primary, false, 8099).is_err());
    fs::write(repository.primary.join(RECORD), bytes).unwrap();
    assert!(load(&repository.primary).is_err());
    assert!(api_port(&repository.primary).is_err());
    assert!(resource_port(&repository.primary).is_err());
}

#[test]
fn dashboard_ports_cannot_reuse_this_or_another_instances_backend_ports() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    for port in [first.api_port, first.resource_port] {
        assert!(resolve(&repository.first, &repository.primary, false, port).is_err());
        assert!(resolve(&repository.second, &repository.primary, true, port).is_err());
    }
    assert_eq!(load(&first.managed_home).unwrap().unwrap(), first);
    repository.primary_is_preserved();
}

#[test]
fn changing_a_record_to_a_foreign_managed_path_is_rejected() {
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let mut foreign = first.clone();
    foreign.managed_home = repository.primary.clone();
    fs::write(
        first.managed_home.join(RECORD),
        serde_json::to_vec(&foreign).unwrap(),
    )
    .unwrap();
    assert!(load(&first.managed_home).is_err());
    assert!(resolve(&repository.first, &repository.primary, false, 8098).is_err());
    repository.primary_is_preserved();
}

#[cfg(unix)]
#[test]
fn symlinked_ownership_metadata_is_rejected_without_reading_foreign_state() {
    use std::os::unix::fs::symlink;
    let repository = Repository::new();
    let first = repository.instance(&repository.first, 8098);
    let record = first.managed_home.join(RECORD);
    let outside = repository.root.join("outside-record.json");
    fs::rename(&record, &outside).unwrap();
    symlink(&outside, &record).unwrap();
    let before = fs::read(&outside).unwrap();
    assert!(load(&first.managed_home).is_err());
    assert_eq!(fs::read(&outside).unwrap(), before);
    repository.primary_is_preserved();
}
