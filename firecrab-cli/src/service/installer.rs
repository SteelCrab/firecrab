//! Use the same host installer for the CLI and curl entry points.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use super::{Error, InstallOpts, payload};

fn bundled_script() -> String {
    let source = include_str!("../../../install.sh").replace("\r\n", "\n");
    let (before, rest) = source.split_once("# BEGIN RELEASE_HELPERS\n").unwrap();
    let (_, after) = rest.split_once("# END RELEASE_HELPERS\n").unwrap();
    format!(
        "{before}{}\n{after}",
        include_str!("../../../scripts/firecrab-release.sh").replace("\r\n", "\n")
    )
}

fn arguments(opts: &InstallOpts) -> Vec<OsString> {
    let mut args = Vec::new();
    for (flag, value) in [("--version", &opts.version), ("--libc", &opts.libc)] {
        if let Some(value) = value {
            args.extend([flag.into(), value.into()]);
        }
    }
    for (flag, value) in [
        ("--bin-dir", &opts.bin_dir),
        ("--dashboard-dir", &opts.dashboard_dir),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value.as_os_str().to_owned()]);
        }
    }
    for (flag, enabled) in [
        ("--no-deps", opts.no_deps),
        ("--no-frontend", opts.no_frontend),
        ("--check", opts.check),
    ] {
        if enabled {
            args.push(flag.into());
        }
    }
    args
}

fn execute(script: &Path, args: &[OsString]) -> Result<(), Error> {
    // Inherit the terminal so package progress and sudo prompts stay visible.
    let status = Command::new("bash")
        .arg(script)
        .args(args)
        .status()
        .map_err(|error| Error::step("install", error.to_string()))?;
    if !status.success() {
        return Err(Error::step(
            "install",
            format!("host installer exited with {status}"),
        ));
    }
    Ok(())
}

pub(super) fn run(opts: InstallOpts, reinstall: bool, purge: bool) -> Result<(), Error> {
    let mut temp = tempfile::NamedTempFile::new()
        .map_err(|error| Error::step("install", error.to_string()))?;
    temp.write_all(bundled_script().as_bytes())
        .map_err(|error| Error::step("install", error.to_string()))?;
    let checkout = match &opts.bin_dir {
        Some(bin) => {
            let absolute = std::fs::canonicalize(bin)
                .map_err(|error| Error::step("payload", format!("{}: {error}", bin.display())))?;
            Some(
                payload::find_checkout(&absolute)
                    .ok_or_else(|| Error::step("payload", "--bin-dir needs a Firecrab checkout"))?,
            )
        }
        None => None,
    };
    let local_script = checkout.map(|root| root.join("install.sh"));
    let script = local_script.as_deref().unwrap_or(temp.path());
    if reinstall && !opts.check {
        let mut args = vec![OsString::from("--uninstall")];
        if purge {
            args.push("--purge".into());
        }
        execute(script, &args)?;
    }
    execute(script, &arguments(&opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_installer_runs_without_a_checkout_or_network() {
        let mut script = tempfile::NamedTempFile::new().unwrap();
        script.write_all(bundled_script().as_bytes()).unwrap();
        let output = Command::new("bash")
            .arg(script.path())
            .arg("--help")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("ingress is bundled"));
    }
}
