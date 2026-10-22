//! Guest-side Git clone, build, and launch injection shared by catalog and
//! OCI-imported MicroVM templates.

use std::collections::BTreeMap;
use std::path::Path;

use firecrab_api_types::{SourceDeployment, SourceRuntime};

use crate::rootfs::{
    RootfsError, guest_path_exists, remove_from_image, run_debugfs, set_guest_file_mode,
    write_into_image,
};

/// Catalog-image path of the generated clone/build/run script.
const RUNNER_PATH: &str = "/usr/local/sbin/firecrab-source-deploy.sh";
/// OCI `services.d` path started by Firecrab's injected BusyBox init.
const OCI_RUNNER_PATH: &str = "/etc/firecrab/services.d/source-deployment";
/// Imported OCI Entrypoint/Cmd service displaced by a source deployment.
const OCI_APP_SERVICE: &str = "/etc/firecrab/services.d/app";
/// systemd unit installed on Ubuntu/Rocky-style catalog images.
const SYSTEMD_UNIT: &str = "/etc/systemd/system/firecrab-source-deployment.service";
/// systemd enablement link under `multi-user.target`.
const SYSTEMD_WANTS: &str =
    "/etc/systemd/system/multi-user.target.wants/firecrab-source-deployment.service";
/// OpenRC service installed on Alpine-style catalog images.
const OPENRC_SERVICE: &str = "/etc/init.d/firecrab-source-deployment";
/// OpenRC default-runlevel enablement link.
const OPENRC_RUNLEVEL: &str = "/etc/runlevels/default/firecrab-source-deployment";

/// Injects one source deployment as the VM's primary application service.
///
/// OCI-imported images use Firecrab's `services.d` runner. Catalog images use
/// their native systemd/OpenRC service manager. In both cases all Git and
/// build commands execute only after the guest network is ready.
pub fn install(
    rootfs: &Path,
    deployment: Option<&SourceDeployment>,
    env: &BTreeMap<String, String>,
) -> Result<(), RootfsError> {
    clear(rootfs);
    let Some(deployment) = deployment else {
        return Ok(());
    };

    if guest_path_exists(rootfs, crate::oci::provision::GUEST_TOOLBOX) {
        ensure_guest_dir(rootfs, "/etc/firecrab")?;
        ensure_guest_dir(rootfs, "/etc/firecrab/services.d")?;
        write_executable(
            rootfs,
            OCI_RUNNER_PATH,
            &runner_script(deployment, env, true),
        )?;
        // A source deployment is the primary workload. Do not also launch the
        // imported container image's original Entrypoint/Cmd.
        remove_from_image(rootfs, OCI_APP_SERVICE);
        return Ok(());
    }

    ensure_guest_dir(rootfs, "/usr/local")?;
    ensure_guest_dir(rootfs, "/usr/local/sbin")?;
    write_executable(rootfs, RUNNER_PATH, &runner_script(deployment, env, false))?;

    if guest_path_exists(rootfs, "/etc/systemd/system") {
        write_into_image(rootfs, SYSTEMD_UNIT, systemd_unit().as_bytes())?;
        if guest_path_exists(rootfs, "/etc/systemd/system/multi-user.target.wants") {
            ensure_symlink(rootfs, SYSTEMD_WANTS, SYSTEMD_UNIT)?;
        }
    }
    if guest_path_exists(rootfs, "/etc/init.d")
        && guest_path_exists(rootfs, "/etc/runlevels/default")
    {
        write_executable(rootfs, OPENRC_SERVICE, &openrc_service())?;
        ensure_symlink(rootfs, OPENRC_RUNLEVEL, OPENRC_SERVICE)?;
    }
    Ok(())
}

/// Renders the guest script with every user value represented as quoted data.
fn runner_script(
    deployment: &SourceDeployment,
    env: &BTreeMap<String, String>,
    oci: bool,
) -> String {
    let shebang = if oci {
        format!("#!{} sh", crate::oci::provision::GUEST_TOOLBOX)
    } else {
        "#!/bin/sh".to_owned()
    };
    let repository = posix_quote(&deployment.repository);
    let revision = posix_quote(&deployment.revision);
    let build_command = posix_quote(&deployment.build_command);
    let exports = env
        .iter()
        .map(|(key, value)| format!("export {key}={}\n", posix_quote(value)))
        .collect::<String>();
    let launch = match &deployment.runtime {
        SourceRuntime::Native { run_command } => format!(
            "run_command={}\nlog \"FIRECRAB_SOURCE_RUNNING native\"\nexec /bin/sh -c \"$run_command\"\n",
            posix_quote(run_command)
        ),
        SourceRuntime::Wasm {
            artifact_path,
            args,
        } => {
            let mut command = format!("exec wasmer run {}", posix_quote(artifact_path));
            if !args.is_empty() {
                command.push_str(" --");
                for arg in args {
                    command.push(' ');
                    command.push_str(&posix_quote(arg));
                }
            }
            format!(
                "if ! command -v wasmer >/dev/null 2>&1; then\n  fail missing-wasmer\nfi\n\
                 if [ ! -f {} ]; then\n  fail missing-wasm-artifact\nfi\n\
                 log \"FIRECRAB_SOURCE_RUNNING wasm\"\n{command}\n",
                posix_quote(artifact_path)
            )
        }
    };

    format!(
        r#"{shebang}
set -u
repository={repository}
revision={revision}
build_command={build_command}
workdir=/var/lib/firecrab/source
{exports}

log() {{
  printf '%s\n' "$*"
  printf '%s\n' "$*" >/dev/console 2>/dev/null || true
}}

fail() {{
  log "FIRECRAB_SOURCE_FAILED $1"
  exit 1
}}

if ! command -v git >/dev/null 2>&1; then
  log "FIRECRAB_SOURCE_INFO installing-git"
  if command -v apt-get >/dev/null 2>&1; then
    DEBIAN_FRONTEND=noninteractive apt-get update -qq && \
      DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git ca-certificates
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y -q git ca-certificates
  elif command -v microdnf >/dev/null 2>&1; then
    microdnf -y install git ca-certificates
  elif command -v yum >/dev/null 2>&1; then
    yum install -y -q git ca-certificates
  elif command -v apk >/dev/null 2>&1; then
    apk add --no-cache git ca-certificates
  else
    fail missing-git
  fi || fail git-install
fi

mkdir -p /var/lib/firecrab || fail create-workdir
if [ ! -d "$workdir/.git" ]; then
  rm -rf "$workdir"
  log "FIRECRAB_SOURCE_CLONING $repository"
  git clone --no-checkout -- "$repository" "$workdir" || fail clone
fi
cd "$workdir" || fail enter-workdir
git remote set-url origin "$repository" || fail remote
if [ -n "$revision" ]; then
  log "FIRECRAB_SOURCE_CHECKOUT $revision"
  git fetch --depth 1 origin "$revision" || fail fetch
  git checkout --detach --force FETCH_HEAD || fail checkout
else
  git fetch --depth 1 origin HEAD || fail fetch
  git checkout --detach --force FETCH_HEAD || fail checkout
fi
git clean -fdx || fail clean

log "FIRECRAB_SOURCE_BUILDING"
/bin/sh -c "$build_command" || fail build
log "FIRECRAB_SOURCE_BUILT"
{launch}"#
    )
}

/// Renders the network-ordered systemd service wrapper.
fn systemd_unit() -> String {
    format!(
        r#"[Unit]
Description=Firecrab Git source deployment
After=network-online.target network.target firecrab-network-ready.service
Wants=network-online.target

[Service]
Type=simple
ExecStart={RUNNER_PATH}
Restart=on-failure
RestartSec=3
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
"#
    )
}

/// Renders the network-ordered OpenRC service wrapper.
fn openrc_service() -> String {
    format!(
        r#"#!/sbin/openrc-run
description="Firecrab Git source deployment"
command="{RUNNER_PATH}"
command_background="no"

depend() {{
	need localmount
	after net firewall dhcpcd firecrab-network-ready
}}
"#
    )
}

/// Reuses OCI service quoting so source and image commands share one rule.
fn posix_quote(value: &str) -> String {
    crate::oci::service::posix_quote(value)
}

/// Removes generated launch artifacts before an idempotent reinjection.
fn clear(rootfs: &Path) {
    for path in [
        RUNNER_PATH,
        OCI_RUNNER_PATH,
        SYSTEMD_UNIT,
        SYSTEMD_WANTS,
        OPENRC_SERVICE,
        OPENRC_RUNLEVEL,
    ] {
        remove_from_image(rootfs, path);
    }
}

/// Writes one guest file and applies executable mode bits.
fn write_executable(rootfs: &Path, path: &str, body: &str) -> Result<(), RootfsError> {
    write_into_image(rootfs, path, body.as_bytes())?;
    set_guest_file_mode(rootfs, path, "0100755");
    Ok(())
}

/// Creates a guest directory through debugfs when the image lacks it.
fn ensure_guest_dir(rootfs: &Path, path: &str) -> Result<(), RootfsError> {
    if guest_path_exists(rootfs, path) {
        return Ok(());
    }
    let _ = run_debugfs(rootfs, &format!("mkdir {path}"));
    if guest_path_exists(rootfs, path) {
        Ok(())
    } else {
        Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("debugfs failed to create {path}"),
        })
    }
}

/// Replaces a guest path with an enablement symlink and verifies the result.
fn ensure_symlink(rootfs: &Path, link: &str, target: &str) -> Result<(), RootfsError> {
    remove_from_image(rootfs, link);
    let _ = run_debugfs(rootfs, &format!("symlink {link} {target}"));
    if guest_path_exists(rootfs, link) {
        Ok(())
    } else {
        Err(RootfsError::Specialize {
            path: rootfs.to_owned(),
            detail: format!("debugfs failed to create symlink {link} -> {target}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn native() -> SourceDeployment {
        SourceDeployment {
            repository: "https://github.com/SteelCrab/example.git".to_owned(),
            revision: "main".to_owned(),
            build_command: "cargo build --release".to_owned(),
            runtime: SourceRuntime::Native {
                run_command: "./target/release/example".to_owned(),
            },
        }
    }

    fn mk_ext4() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempdir().unwrap();
        let rootfs = directory.path().join("rootfs.ext4");
        let status = Command::new("mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&rootfs)
            .arg("8M")
            .status()
            .expect("mkfs.ext4 must be installed for this test");
        assert!(status.success(), "mkfs.ext4 failed");
        (directory, rootfs)
    }

    fn mkdir_all(rootfs: &Path, directories: &[&str]) {
        for directory in directories {
            run_debugfs(rootfs, &format!("mkdir {directory}")).unwrap();
        }
    }

    #[test]
    fn native_runner_keeps_commands_as_quoted_data() {
        let mut deployment = native();
        deployment.build_command = "printf '%s' \"$TOKEN\"".to_owned();
        let env = BTreeMap::from([("TOKEN".to_owned(), "a'$b".to_owned())]);
        let script = runner_script(&deployment, &env, false);
        assert!(script.contains("build_command='printf '\\''%s'\\'' \"$TOKEN\"'"));
        assert!(script.contains("export TOKEN='a'\\''$b'"));
        assert!(script.contains("/bin/sh -c \"$build_command\""));
        assert!(script.contains("FIRECRAB_SOURCE_RUNNING native"));
    }

    #[test]
    fn wasm_runner_uses_official_wasmer_run_argument_separator() {
        let deployment = SourceDeployment {
            repository: "https://example.com/app.git".to_owned(),
            revision: String::new(),
            build_command: "cargo build --target wasm32-wasip1".to_owned(),
            runtime: SourceRuntime::Wasm {
                artifact_path: "target/app.wasm".to_owned(),
                args: vec!["--port".to_owned(), "8080".to_owned()],
            },
        };
        let script = runner_script(&deployment, &BTreeMap::new(), false);
        assert!(script.contains("exec wasmer run 'target/app.wasm' -- '--port' '8080'"));
        assert!(script.contains("missing-wasmer"));
    }

    #[test]
    fn wasm_runner_without_args_omits_separator() {
        let deployment = SourceDeployment {
            repository: "https://example.com/app.git".to_owned(),
            revision: String::new(),
            build_command: "cargo build --target wasm32-wasip1".to_owned(),
            runtime: SourceRuntime::Wasm {
                artifact_path: "app.wasm".to_owned(),
                args: Vec::new(),
            },
        };
        let script = runner_script(&deployment, &BTreeMap::new(), true);
        assert!(script.contains("exec wasmer run 'app.wasm'\n"));
        assert!(!script.contains("exec wasmer run 'app.wasm' --"));
        assert!(script.starts_with(&format!("#!{} sh", crate::oci::provision::GUEST_TOOLBOX)));
    }

    #[test]
    fn installs_systemd_service_on_catalog_image_layout() {
        let (_directory, rootfs) = mk_ext4();
        mkdir_all(
            &rootfs,
            &[
                "/etc",
                "/etc/systemd",
                "/etc/systemd/system",
                "/etc/systemd/system/multi-user.target.wants",
                "/usr",
                "/usr/local",
            ],
        );
        install(&rootfs, Some(&native()), &BTreeMap::new()).unwrap();
        assert!(guest_path_exists(&rootfs, RUNNER_PATH));
        assert!(guest_path_exists(&rootfs, SYSTEMD_UNIT));
        assert!(guest_path_exists(&rootfs, SYSTEMD_WANTS));
    }

    #[test]
    fn oci_source_deployment_replaces_original_entrypoint_service() {
        let (_directory, rootfs) = mk_ext4();
        mkdir_all(
            &rootfs,
            &["/etc", "/etc/firecrab", "/etc/firecrab/services.d"],
        );
        write_into_image(
            rootfs.as_path(),
            crate::oci::provision::GUEST_TOOLBOX,
            b"busybox",
        )
        .unwrap();
        write_into_image(
            rootfs.as_path(),
            OCI_APP_SERVICE,
            b"#!/bin/sh\nexec old-app\n",
        )
        .unwrap();

        install(&rootfs, Some(&native()), &BTreeMap::new()).unwrap();
        assert!(guest_path_exists(&rootfs, OCI_RUNNER_PATH));
        assert!(!guest_path_exists(&rootfs, OCI_APP_SERVICE));
        assert!(!guest_path_exists(&rootfs, SYSTEMD_UNIT));
    }

    #[test]
    fn installs_openrc_service_on_alpine_layout() {
        let (_directory, rootfs) = mk_ext4();
        mkdir_all(
            &rootfs,
            &[
                "/etc",
                "/etc/init.d",
                "/etc/runlevels",
                "/etc/runlevels/default",
                "/usr",
                "/usr/local",
            ],
        );
        install(&rootfs, Some(&native()), &BTreeMap::new()).unwrap();
        assert!(guest_path_exists(&rootfs, RUNNER_PATH));
        assert!(guest_path_exists(&rootfs, OPENRC_SERVICE));
        assert!(guest_path_exists(&rootfs, OPENRC_RUNLEVEL));
    }

    #[test]
    fn install_none_clears_previous_catalog_artifacts() {
        let (_directory, rootfs) = mk_ext4();
        mkdir_all(
            &rootfs,
            &[
                "/etc",
                "/etc/systemd",
                "/etc/systemd/system",
                "/etc/systemd/system/multi-user.target.wants",
                "/usr",
                "/usr/local",
            ],
        );
        install(&rootfs, Some(&native()), &BTreeMap::new()).unwrap();
        install(&rootfs, None, &BTreeMap::new()).unwrap();
        assert!(!guest_path_exists(&rootfs, RUNNER_PATH));
        assert!(!guest_path_exists(&rootfs, SYSTEMD_UNIT));
        assert!(!guest_path_exists(&rootfs, SYSTEMD_WANTS));
    }
}
