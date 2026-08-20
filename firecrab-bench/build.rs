use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    for name in [
        "FIRECRAB_BUILD_COMMIT",
        "FIRECRAB_BUILD_BRANCH",
        "FIRECRAB_BUILD_DIRTY",
        "GITHUB_SHA",
        "GITHUB_HEAD_REF",
        "GITHUB_REF_NAME",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let repository = Path::new(&env::var("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .parent()
        .expect("firecrab-bench belongs to the workspace")
        .to_path_buf();
    track_git_identity(&repository);

    let commit = first_environment_value(&["FIRECRAB_BUILD_COMMIT", "GITHUB_SHA"])
        .or_else(|| git_output(&repository, &["rev-parse", "HEAD"]))
        .filter(|value| is_commit(value))
        .unwrap_or_else(|| "unknown".to_owned());
    let branch = first_environment_value(&[
        "FIRECRAB_BUILD_BRANCH",
        "GITHUB_HEAD_REF",
        "GITHUB_REF_NAME",
    ])
    .or_else(|| git_output(&repository, &["symbolic-ref", "--short", "-q", "HEAD"]))
    .filter(|value| is_safe_value(value))
    .unwrap_or_else(|| "unknown".to_owned());
    let dirty = first_environment_value(&["FIRECRAB_BUILD_DIRTY"])
        .and_then(|value| parse_bool(&value))
        .unwrap_or_else(|| git_is_dirty(&repository));

    println!("cargo:rustc-env=FIRECRAB_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=FIRECRAB_BUILD_BRANCH={branch}");
    println!("cargo:rustc-env=FIRECRAB_BUILD_DIRTY={dirty}");
}

fn first_environment_value(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()))
}

fn git_output(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_is_dirty(repository: &Path) -> bool {
    git_output(
        repository,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .is_some()
}

fn track_git_identity(repository: &Path) {
    let Some(head) = git_output(repository, &["rev-parse", "--git-path", "HEAD"]) else {
        return;
    };
    let head = resolve_path(repository, &head);
    println!("cargo:rerun-if-changed={}", head.display());
    let Ok(contents) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(reference) = contents.trim().strip_prefix("ref: ") else {
        return;
    };
    if let Some(path) = git_output(repository, &["rev-parse", "--git-path", reference]) {
        println!(
            "cargo:rerun-if-changed={}",
            resolve_path(repository, &path).display()
        );
    }
}

fn resolve_path(repository: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        repository.join(path)
    }
}

fn is_commit(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_safe_value(value: &str) -> bool {
    value.len() <= 255 && !value.chars().any(char::is_control)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}
