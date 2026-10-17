//! Imports the managed distribution and provisions Firecrab inside it.
//!
//! The guest steps mirror the macOS provisioner: the same pinned Firecracker
//! and Firecrab release, the same `install.sh --no-deps`, and the same nested
//! Firecracker gate before anything is reported as installed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::artifact::{self, ArtifactSpec, HashAlgorithm};
use super::super::manifest::{self, Manifest};
use super::lifecycle::Layout;
use super::wsl::{self, DISTRO_NAME};

// Frozen pins validate legacy installations. Explicit installs use the TOML.
pub const DEBIAN_ROOTFS_VERSION: &str = "1.26.0.0";
pub const FIRECRACKER_VERSION: &str = "v1.17.0";
pub const FIRECRAB_VERSION: &str = "v0.2.2";

/// Bumped whenever the guest steps change, so an older marker forces a re-run.
const PROVISION_SCHEMA: u32 = 1;

const INSTALLER: ArtifactSpec<'static> = ArtifactSpec {
    label: "Firecrab guest installer",
    filename: "install-firecrab-v0.2.2.sh",
    url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/install.sh",
    algorithm: HashAlgorithm::Sha256,
    digest: "af2fb56b92dff1559cdaa449aa025808e0990437c16ccc0bb2b4d558ef50dacb",
};

/// Everything that differs between x86_64 and ARM64 Windows hosts.
#[derive(Clone)]
pub struct Host<'a> {
    pub architecture: &'static str,
    /// Suffix of Debian's `linux-image-*` metapackage.
    debian_architecture: &'static str,
    artifacts: [ArtifactSpec<'a>; 4],
    pub rootfs_version: &'a str,
    pub firecracker_version: &'a str,
    pub firecrab_version: &'a str,
    pub kernel_version: Option<&'a str>,
    distribution: Option<(u32, &'a str)>,
    manifest_id: Option<String>,
    /// `install.sh` places this helper; it turns Debian's vmlinuz into what
    /// Firecracker boots on this architecture.
    kernel_extractor: &'static str,
    boot_args: &'static str,
    /// x86_64 Firecracker exits on a guest reboot; ARM64 also on power-off.
    workload_exit: &'static str,
}

/// Microsoft's WSL manifest points `wsl --install -d Debian` at these Debian
/// builds, so the managed distribution is the same image users already get.
static X86_64: Host<'static> = Host {
    architecture: "x86_64",
    rootfs_version: DEBIAN_ROOTFS_VERSION,
    firecracker_version: FIRECRACKER_VERSION,
    firecrab_version: FIRECRAB_VERSION,
    kernel_version: None,
    distribution: None,
    manifest_id: None,
    debian_architecture: "amd64",
    artifacts: [
        ArtifactSpec {
            label: "Debian 13 WSL rootfs",
            filename: "Debian_WSL_AMD64_v1.26.0.0.wsl",
            url: concat!(
                "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/",
                "Debian_WSL_AMD64_v1.26.0.0.wsl"
            ),
            algorithm: HashAlgorithm::Sha256,
            digest: "5ec7dc68216e75d1d4d4761474e99d8461a98d316537110314b137122a879e0f",
        },
        ArtifactSpec {
            label: "Firecracker x86_64 release",
            filename: "firecracker-v1.17.0-x86_64.tgz",
            url: "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz",
            algorithm: HashAlgorithm::Sha256,
            digest: "06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558",
        },
        ArtifactSpec {
            label: "Firecrab x86_64 GNU host bundle",
            filename: "firecrab-host-x86_64-gnu.tar.gz",
            url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/firecrab-host-x86_64-gnu.tar.gz",
            algorithm: HashAlgorithm::Sha256,
            digest: "ad59b359ccc6d4f28ca6927339b6157b7684b9d734ded79c639a546d709b5315",
        },
        INSTALLER,
    ],
    kernel_extractor: "extract-vmlinux",
    boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/init",
    workload_exit: "reboot -f",
};

static ARM64: Host<'static> = Host {
    architecture: "aarch64",
    rootfs_version: DEBIAN_ROOTFS_VERSION,
    firecracker_version: FIRECRACKER_VERSION,
    firecrab_version: FIRECRAB_VERSION,
    kernel_version: None,
    distribution: None,
    manifest_id: None,
    debian_architecture: "arm64",
    artifacts: [
        ArtifactSpec {
            label: "Debian 13 WSL rootfs",
            filename: "Debian_WSL_ARM64_v1.26.0.0.wsl",
            url: concat!(
                "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/",
                "Debian_WSL_ARM64_v1.26.0.0.wsl"
            ),
            algorithm: HashAlgorithm::Sha256,
            digest: "09120df4fadc36fb2a0f7298e197785e6f599f12aaf95d43cba07a8ac7fb316b",
        },
        ArtifactSpec {
            label: "Firecracker ARM64 release",
            filename: "firecracker-v1.17.0-aarch64.tgz",
            url: "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-aarch64.tgz",
            algorithm: HashAlgorithm::Sha256,
            digest: "e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256",
        },
        ArtifactSpec {
            label: "Firecrab ARM64 GNU host bundle",
            filename: "firecrab-host-aarch64-gnu.tar.gz",
            url: "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/firecrab-host-aarch64-gnu.tar.gz",
            algorithm: HashAlgorithm::Sha256,
            digest: "2e995ed27d8c19848baaf38c8bf8a2d5d126ab90776738d91790524a8564e982",
        },
        INSTALLER,
    ],
    kernel_extractor: "extract-arm64-image",
    boot_args: "keep_bootcon console=ttyS0 reboot=k panic=1 root=/dev/vda rw init=/sbin/init",
    workload_exit: "poweroff -f",
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("microManager supports x86_64 and ARM64 Windows hosts, not {0}")]
    UnsupportedArchitecture(&'static str),
    #[error(transparent)]
    Artifact(#[from] artifact::Error),
    #[error(transparent)]
    Manifest(#[from] manifest::Error),
    #[error(transparent)]
    Wsl(#[from] wsl::Error),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("guest provisioning failed: {0}")]
    GuestProvision(String),
    #[error("guest provisioning stopped without a success or failure marker; see {0}")]
    MissingMarker(PathBuf),
}

/// The pinned artifacts for the architecture this CLI was built for.
pub fn host() -> Result<&'static Host<'static>, Error> {
    host_for(std::env::consts::ARCH).ok_or(Error::UnsupportedArchitecture(std::env::consts::ARCH))
}

fn host_for(architecture: &str) -> Option<&'static Host<'static>> {
    match architecture {
        "x86_64" => Some(&X86_64),
        "aarch64" => Some(&ARM64),
        _ => None,
    }
}

pub fn host_from_manifest<'a>(
    manifest: &'a Manifest,
    architecture: &'static str,
) -> Result<Host<'a>, Error> {
    let mut host: Host<'a> = host_for(architecture)
        .ok_or(Error::UnsupportedArchitecture(architecture))?
        .clone();
    let (image, firecracker, firecrab) = match architecture {
        "x86_64" => (
            &manifest.windows.x86_64,
            &manifest.firecracker.x86_64,
            &manifest.firecrab.x86_64,
        ),
        "aarch64" => (
            &manifest.windows.aarch64,
            &manifest.firecracker.aarch64,
            &manifest.firecrab.aarch64,
        ),
        _ => unreachable!("architecture checked above"),
    };
    host.artifacts = [
        image.image.spec("Debian WSL rootfs"),
        firecracker.spec("Firecracker release"),
        firecrab.spec("Firecrab GNU host bundle"),
        manifest.firecrab.installer.spec("Firecrab guest installer"),
    ];
    host.rootfs_version = &manifest.windows.rootfs_version;
    host.firecracker_version = &manifest.firecracker.version;
    host.firecrab_version = &manifest.firecrab.version;
    host.kernel_version = Some(&image.kernel_version);
    host.distribution = Some((manifest.debian.version, &manifest.debian.suite));
    host.manifest_id = Some(manifest.fingerprint()?);
    Ok(host)
}

pub struct Downloaded {
    pub debian_rootfs: PathBuf,
    pub firecracker: PathBuf,
    pub firecrab_host: PathBuf,
    pub installer: PathBuf,
}

pub fn download_all(host: &Host, directory: &Path, assume_yes: bool) -> Result<Downloaded, Error> {
    artifact::fetch_all(&host.artifacts, directory, assume_yes)?;
    let [debian, firecracker, firecrab, installer] = &host.artifacts;
    Ok(Downloaded {
        debian_rootfs: directory.join(debian.filename),
        firecracker: directory.join(firecracker.filename),
        firecrab_host: directory.join(firecrab.filename),
        installer: directory.join(installer.filename),
    })
}

/// Re-hashes every cached artifact without touching the network.
pub fn validate_downloads(
    host: &Host,
    directory: &Path,
) -> Vec<(&'static str, Result<(), artifact::Error>)> {
    host.artifacts
        .iter()
        .map(|spec| {
            let path = directory.join(spec.filename);
            (
                spec.label,
                artifact::verify(&path, spec.algorithm, spec.digest),
            )
        })
        .collect()
}

/// Imports the managed distribution unless WSL already has it. Returns whether
/// it was imported, because a fresh distribution has never been provisioned.
pub fn ensure_distro(layout: &Layout, rootfs: &Path) -> Result<bool, Error> {
    if wsl::contains(&wsl::distributions(), DISTRO_NAME) {
        println!("[PASS] distribution: {DISTRO_NAME} (preserved)");
        return Ok(false);
    }
    wsl::run(&[
        "--import",
        DISTRO_NAME,
        &layout.distro().to_string_lossy(),
        &rootfs.to_string_lossy(),
        "--version",
        "2",
    ])?;
    println!("[PASS] distribution: {DISTRO_NAME} imported");
    Ok(true)
}

/// Runs the guest provisioner unless a current marker says it already passed.
pub fn ensure_provisioned(layout: &Layout, host: &Host, force: bool) -> Result<String, Error> {
    let share = wsl::guest_path(&layout.managed_home)?;
    provision(layout, host, force, &share)
}

/// `share` is the managed home as the guest sees it under `/mnt`.
fn provision(layout: &Layout, host: &Host, force: bool, share: &str) -> Result<String, Error> {
    // A failed run is retried rather than reported again, as on macOS.
    remove_if_present(&failure_marker(layout))?;
    if !force && let Some(result) = guest_result(layout)? {
        let matches = host.manifest_id.as_deref().is_none_or(|id| {
            manifest::marker_value_matches(&result, "manifest", id)
                && host.kernel_version.is_some_and(|kernel| {
                    manifest::marker_value_matches(&result, "workload_kernel", kernel)
                })
        });
        if matches {
            println!("[PASS] guest: Firecrab provisioning (preserved)");
            return Ok(result);
        }
    }
    let directory = layout.provision();
    fs::create_dir_all(&directory).map_err(|source| Error::Io {
        action: "create provisioning directory",
        path: directory,
        source,
    })?;
    write_script(
        &layout.provision().join("guest-provision.sh"),
        &guest_provision_script(host, share),
    )?;
    write_script(
        &layout.provision().join("nested-firecracker-e2e.sh"),
        &nested_firecracker_e2e_script(host),
    )?;
    remove_if_present(&layout.provision_marker())?;

    println!(
        "[BOOT] {DISTRO_NAME}: apt, Firecracker, Firecrab, and the nested gate take several minutes"
    );
    let script = format!("{share}/provision/guest-provision.sh");
    let run = wsl::run(&["-d", DISTRO_NAME, "-u", "root", "--exec", "bash", &script]);
    let result = guest_result(layout)?;
    match (run, result) {
        (Ok(_), Some(result)) => {
            println!("[PASS] guest: Firecrab provisioning");
            Ok(result)
        }
        (Err(error), None) => Err(error.into()),
        (Ok(_), None) => Err(Error::MissingMarker(guest_log(layout))),
        (Err(_), Some(_)) => Err(Error::GuestProvision(format!(
            "the provisioner exited non-zero after writing its marker; see {}",
            guest_log(layout).display()
        ))),
    }
}

/// Reads the provisioning outcome: `None` before the first run, an error
/// naming the failed phase, or the marker once every gate has passed.
pub fn guest_result(layout: &Layout) -> Result<Option<String>, Error> {
    let failed = failure_marker(layout);
    if failed.is_file() {
        let code = read(&failed)?;
        let phase = read(&layout.runtime().join("provision.phase")).unwrap_or_default();
        return Err(Error::GuestProvision(format!(
            "phase {} exited with {}; see {}",
            phase.trim(),
            code.trim(),
            guest_log(layout).display()
        )));
    }
    let marker = layout.provision_marker();
    if !marker.is_file() {
        return Ok(None);
    }
    let result = read(&marker)?;
    let expected_schema = format!("schema={PROVISION_SCHEMA}");
    for required in [
        expected_schema.as_str(),
        "kvm=usable",
        "firecrab_api=active",
        "firecrab_net_helper=active",
        "nested_firecracker=passed",
    ] {
        if !result.lines().any(|line| line == required) {
            // An older schema is not a failure; it just needs another run.
            return Ok(None);
        }
    }
    Ok(Some(result))
}

pub fn guest_log(layout: &Layout) -> PathBuf {
    layout.runtime().join("guest-provision.log")
}

fn failure_marker(layout: &Layout) -> PathBuf {
    layout.runtime().join("provision.failed")
}

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::Io {
        action: "read",
        path: path.to_path_buf(),
        source,
    })
}

fn remove_if_present(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            action: "remove",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The guest runs these with bash, which rejects the `\r` a CRLF file would add.
fn write_script(path: &Path, contents: &str) -> Result<(), Error> {
    fs::write(path, contents).map_err(|source| Error::Io {
        action: "write",
        path: path.to_path_buf(),
        source,
    })
}

fn guest_provision_script(host: &Host, share: &str) -> String {
    format!(
        r#"#!/bin/bash
set -Eeuxo pipefail
share={share}
log=/var/log/firecrab-provision.log
exec >>"$log" 2>&1
failed="$share/runtime/provision.failed"
ready="$share/runtime/provisioned"
phase_file="$share/runtime/provision.phase"
rm -f "$failed" "$ready"
phase() {{ echo "$1" >"$phase_file"; }}
finish() {{
  rc=$?
  set +e
  cp -f "$log" "$share/runtime/guest-provision.log"
  if [ "$rc" -ne 0 ]; then echo "$rc" >"$failed"; fi
}}
trap finish EXIT

{check_distribution}

phase kvm
# kvm_intel loads about 25 s into a cold WSL2 boot.
for _ in $(seq 1 120); do
  [ -c /dev/kvm ] && break
  sleep 1
done
if [ ! -c /dev/kvm ]; then
  if [ -e /dev/kvm ]; then echo '/dev/kvm is a regular file; run wsl --shutdown' >&2; fi
  exit 1
fi

phase apt-update
export DEBIAN_FRONTEND=noninteractive
apt-get update
phase apt-install
{clean_guest_installation}
apt-get install -y ca-certificates curl sudo tar findutils xz-utils gzip zstd lz4 lzop \
  nftables dnsmasq dnsmasq-utils iproute2 jq e2fsprogs fakeroot busybox-static socat \
  binutils initramfs-tools {kernel_package}

phase firecracker
tmp=$(mktemp -d)
tar -xzf "$share/downloads/{firecracker_archive}" -C "$tmp"
fc=$(find "$tmp" -type f -name 'firecracker-{firecracker}-{architecture}' -print -quit)
jailer=$(find "$tmp" -type f -name 'jailer-{firecracker}-{architecture}' -print -quit)
test -n "$fc"
install -m 0755 "$fc" /usr/local/bin/firecracker
if [ -n "$jailer" ]; then install -m 0755 "$jailer" /usr/local/bin/jailer; fi
rm -rf "$tmp"

phase firecrab
bash "$share/downloads/{installer}" --no-deps
(systemctl is-active --quiet firecrab-helper || systemctl is-active --quiet firecrab-net-helper)
systemctl is-active --quiet firecrab-api

phase nested-firecracker
bash "$share/provision/nested-firecracker-e2e.sh"

phase verify
{{
  echo 'schema={schema}'
  {manifest_marker}
  echo "kernel=$(uname -r)"
  {workload_kernel_marker}
  echo "debian=$(. /etc/os-release; echo "$PRETTY_NAME")"
  if [ -c /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    echo 'kvm=usable'
  else
    echo 'kvm=unusable'
  fi
  echo 'firecrab_api=active'
  echo 'firecrab_net_helper=active'
  echo 'nested_firecracker=passed'
}} >"$ready"
phase complete
exit 0
"#,
        share = wsl::shell_quote(share),
        check_distribution = host.distribution.map(|(version, suite)| format!(
            "phase distribution\n. /etc/os-release\ntest \"$ID\" = debian\ntest \"$VERSION_ID\" = '{version}'\ntest \"$VERSION_CODENAME\" = '{suite}'"
        )).unwrap_or_default(),
        kernel_package = match host.kernel_version {
            Some(version) => format!("linux-image-{version}"),
            None => format!("linux-image-{}", host.debian_architecture),
        },
        architecture = host.architecture,
        clean_guest_installation = if host.manifest_id.is_some() {
            r#"for unit in firecrab-api firecrab-helper firecrab-net-helper; do
  rm -f "/etc/systemd/system/$unit.service.d/90-firecrab-dev.conf"
done
systemctl daemon-reload"#
        } else {
            ""
        },
        firecracker = host.firecracker_version,
        firecracker_archive = host.artifacts[1].filename,
        installer = host.artifacts[3].filename,
        manifest_marker = host
            .manifest_id
            .as_ref()
            .map(|id| format!("echo 'manifest={id}'"))
            .unwrap_or_default(),
        workload_kernel_marker = host
            .kernel_version
            .map(|version| format!("echo 'workload_kernel={version}'"))
            .unwrap_or_default(),
        schema = PROVISION_SCHEMA,
    )
}

/// Boots a busybox workload under the guest's own Debian kernel. WSL runs
/// Microsoft's kernel, so the kernel comes from the `linux-image-*` package
/// rather than `uname -r`.
fn nested_firecracker_e2e_script(host: &Host) -> String {
    format!(
        r#"#!/bin/bash
set -Eeuxo pipefail
work=/var/lib/firecrab/e2e-nested
rm -rf "$work"
mkdir -p "$work/rootfs"/{{bin,sbin,proc,sys,dev,etc,tmp}}
cp /bin/busybox "$work/rootfs/bin/busybox"
ln -s busybox "$work/rootfs/bin/sh"
cat >"$work/rootfs/sbin/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mount -t proc proc /proc || true
/bin/busybox mount -t sysfs sysfs /sys || true
/bin/busybox echo FIRECRAB_NESTED_WORKLOAD_OK
/bin/busybox sync
/bin/busybox {workload_exit}
INIT
chmod 0755 "$work/rootfs/sbin/init"
truncate -s 128M "$work/rootfs.ext4"
mkfs.ext4 -q -F -d "$work/rootfs" "$work/rootfs.ext4"
{select_kernel}
kver=${{vmlinuz#/boot/vmlinuz-}}
test -f "/boot/initrd.img-$kver"
/usr/local/lib/firecrab/{kernel_extractor} "$vmlinuz" >"$work/kernel"
test -s "$work/kernel"
cat >"$work/config.json" <<JSON
{{
  "boot-source": {{
    "kernel_image_path": "$work/kernel",
    "initrd_path": "/boot/initrd.img-$kver",
    "boot_args": "{boot_args}"
  }},
  "drives": [{{
    "drive_id": "rootfs",
    "path_on_host": "$work/rootfs.ext4",
    "is_root_device": true,
    "is_read_only": false
  }}],
  "machine-config": {{
    "vcpu_count": 1,
    "mem_size_mib": 256
  }}
}}
JSON
rm -f "$work/firecracker.sock" "$work/console.log"
set +e
# A WSL2 guest nests one level deeper than macOS, so boots are slower.
timeout 300 /usr/local/bin/firecracker \
  --api-sock "$work/firecracker.sock" \
  --config-file "$work/config.json" </dev/null >"$work/console.log" 2>&1
rc=$?
set -e
cat "$work/console.log"
test "$rc" -eq 0
grep -a -q FIRECRAB_NESTED_WORKLOAD_OK "$work/console.log"
echo passed >"$work/result"
"#,
        workload_exit = host.workload_exit,
        kernel_extractor = host.kernel_extractor,
        select_kernel = match host.kernel_version {
            Some(version) => format!("vmlinuz=/boot/vmlinuz-{version}"),
            None => "vmlinuz=$(ls -1 /boot/vmlinuz-* | sort -V | tail -n 1)".to_string(),
        },
        boot_args = host.boot_args,
    )
}

#[cfg(test)]
mod tests {
    use super::super::wsl::fake;
    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let directory = tempfile::tempdir().expect("temp dir");
        let layout = Layout {
            managed_home: directory.path().join("micromanager"),
        };
        super::super::lifecycle::prepare(&layout).expect("managed directories");
        (directory, layout)
    }

    fn passing_marker() -> String {
        format!(
            "schema={PROVISION_SCHEMA}\nkvm=usable\nfirecrab_api=active\nfirecrab_net_helper=active\nnested_firecracker=passed\n"
        )
    }

    #[test]
    fn both_windows_architectures_have_pinned_artifacts() {
        for (architecture, marker) in [("x86_64", "AMD64"), ("aarch64", "ARM64")] {
            let host = host_for(architecture).expect("supported");
            assert_eq!(host.architecture, architecture);
            assert!(host.artifacts[0].filename.contains(marker));
            for spec in &host.artifacts {
                assert_eq!(spec.algorithm, HashAlgorithm::Sha256, "{}", spec.label);
                assert_eq!(spec.digest.len(), 64, "{} digest length", spec.label);
                assert!(spec.digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(spec.url.starts_with("https://"), "{} scheme", spec.label);
                assert!(spec.url.ends_with(spec.filename) || spec.filename == INSTALLER.filename);
            }
            assert!(
                host.artifacts[0]
                    .url
                    .contains("salsa.debian.org/debian/WSL/")
            );
            assert!(host.artifacts[0].url.contains(DEBIAN_ROOTFS_VERSION));
            assert!(host.artifacts[1].url.contains(FIRECRACKER_VERSION));
            assert!(host.artifacts[1].filename.contains(architecture));
            assert!(host.artifacts[2].url.contains(FIRECRAB_VERSION));
            assert!(host.artifacts[2].filename.contains(architecture));
        }
        assert!(host_for("x86").is_none());
    }

    #[test]
    fn validation_reports_every_artifact_without_downloading() {
        let (_directory, layout) = layout();
        let results = validate_downloads(&X86_64, &layout.downloads());
        assert_eq!(results.len(), X86_64.artifacts.len());
        assert!(
            results.iter().all(|(_, result)| result.is_err()),
            "nothing is cached"
        );
    }

    const SHARE: &str = "/mnt/c/Users/dev/AppData/Local/Firecrab/micromanager";

    /// Plays the guest: the provisioner's only outputs are the marker files.
    fn guest(
        layout: &Layout,
        outcome: Result<String, (&'static str, &'static str)>,
    ) -> fake::Guard {
        let runtime = layout.runtime();
        fake::answer(move |line| {
            assert_eq!(
                line,
                format!(
                    "wsl.exe -d firecrab-debian -u root --exec bash {SHARE}/provision/guest-provision.sh"
                )
            );
            match &outcome {
                Ok(marker) => {
                    fs::write(runtime.join("provisioned"), marker).expect("marker");
                    Ok(String::new())
                }
                Err((phase, code)) => {
                    fs::write(runtime.join("provision.phase"), phase).expect("phase");
                    fs::write(runtime.join("provision.failed"), code).expect("failure");
                    Err(format!("exit {code}"))
                }
            }
        })
    }

    #[test]
    fn a_fresh_distribution_is_imported_into_the_managed_home() {
        let (_directory, layout) = layout();
        let wsl = fake::answer(|line| {
            if line == "wsl.exe --list --quiet" {
                return Ok("Debian\n".into());
            }
            Ok(String::new())
        });
        let rootfs = layout.downloads().join("rootfs.wsl");
        assert!(ensure_distro(&layout, &rootfs).expect("imported"));
        assert_eq!(
            wsl.calls()[1],
            format!(
                "wsl.exe --import firecrab-debian {} {} --version 2",
                layout.distro().display(),
                rootfs.display()
            )
        );
    }

    #[test]
    fn an_existing_distribution_is_preserved() {
        let (_directory, layout) = layout();
        let wsl = fake::answer(|_| Ok("Debian\nfirecrab-debian\n".into()));
        assert!(!ensure_distro(&layout, Path::new("unused")).expect("preserved"));
        assert_eq!(wsl.calls().len(), 1, "no import");
    }

    #[test]
    fn provisioning_writes_both_scripts_and_returns_the_marker() {
        let (_directory, layout) = layout();
        let wsl = guest(&layout, Ok(passing_marker()));
        let result = provision(&layout, &X86_64, false, SHARE).expect("provisioned");
        assert!(result.contains("nested_firecracker=passed"));
        assert!(layout.provision().join("guest-provision.sh").is_file());
        assert!(
            layout
                .provision()
                .join("nested-firecracker-e2e.sh")
                .is_file()
        );
        assert_eq!(wsl.calls().len(), 1);
        drop(wsl);

        let _untouched = fake::answer(|line| panic!("a passing marker is preserved: {line}"));
        provision(&layout, &X86_64, false, SHARE).expect("preserved");
    }

    #[test]
    fn a_forced_run_replaces_a_passing_marker() {
        let (_directory, layout) = layout();
        fs::write(layout.provision_marker(), passing_marker()).expect("marker");
        let wsl = guest(&layout, Ok(passing_marker()));
        provision(&layout, &X86_64, true, SHARE).expect("provisioned again");
        assert_eq!(wsl.calls().len(), 1);
    }

    #[test]
    fn a_failed_phase_is_reported_and_retried_next_time() {
        let (_directory, layout) = layout();
        let _wsl = guest(&layout, Err(("apt-install", "100")));
        let Err(Error::GuestProvision(detail)) = provision(&layout, &X86_64, false, SHARE) else {
            panic!("the failure marker names the phase");
        };
        assert!(
            detail.contains("phase apt-install exited with 100"),
            "{detail}"
        );

        let _retry = guest(&layout, Ok(passing_marker()));
        provision(&layout, &X86_64, false, SHARE).expect("the next run starts clean");
    }

    #[test]
    fn a_provisioner_that_leaves_no_marker_is_an_error() {
        let (_directory, layout) = layout();
        let _silent = fake::answer(|_| Ok(String::new()));
        assert!(matches!(
            provision(&layout, &X86_64, false, SHARE),
            Err(Error::MissingMarker(_))
        ));
        let _broken = fake::answer(|_| Err("WSL could not start".into()));
        assert!(matches!(
            provision(&layout, &X86_64, false, SHARE),
            Err(Error::Wsl(_))
        ));
    }

    #[test]
    fn an_unmappable_managed_home_is_refused_before_anything_runs() {
        // A share path has no drive letter on any host, so WSL cannot mount it.
        let layout = Layout {
            managed_home: PathBuf::from("\\\\server\\share\\micromanager"),
        };
        let _wsl = fake::answer(|line| panic!("nothing may run: {line}"));
        assert!(matches!(
            ensure_provisioned(&layout, &X86_64, true),
            Err(Error::Wsl(wsl::Error::UnmappablePath(_)))
        ));
    }

    #[test]
    fn no_marker_means_never_provisioned() {
        let (_directory, layout) = layout();
        assert!(guest_result(&layout).expect("readable").is_none());
    }

    #[test]
    fn a_complete_marker_is_accepted() {
        let (_directory, layout) = layout();
        fs::write(layout.provision_marker(), passing_marker()).expect("marker");
        assert!(guest_result(&layout).expect("readable").is_some());
    }

    #[test]
    fn an_older_or_partial_marker_asks_for_another_run() {
        let (_directory, layout) = layout();
        fs::write(layout.provision_marker(), "schema=0\nkvm=usable\n").expect("marker");
        assert!(guest_result(&layout).expect("readable").is_none());
    }

    #[test]
    fn a_failure_marker_names_the_phase_and_the_log() {
        let (_directory, layout) = layout();
        fs::write(layout.provision_marker(), passing_marker()).expect("marker");
        fs::write(failure_marker(&layout), "100\n").expect("failure");
        fs::write(layout.runtime().join("provision.phase"), "apt-install\n").expect("phase");
        let Err(Error::GuestProvision(detail)) = guest_result(&layout) else {
            panic!("a failure marker wins over a stale success marker");
        };
        assert!(
            detail.contains("phase apt-install exited with 100"),
            "{detail}"
        );
        assert!(detail.contains("guest-provision.log"), "{detail}");
    }

    #[test]
    fn explicit_toml_install_reprovisions_a_legacy_marker_once_without_reimport() {
        for architecture in ["x86_64", "aarch64"] {
            let (_directory, layout) = layout();
            fs::write(layout.provision_marker(), passing_marker()).unwrap();
            let disk = layout.distro().join("ext4.vhdx");
            fs::write(&disk, b"keep distribution").unwrap();
            let manifest = Manifest::bundled().unwrap();
            let host = host_from_manifest(&manifest, architecture).unwrap();
            let marker = format!(
                "{}manifest={}\nworkload_kernel={}\n",
                passing_marker(),
                manifest.fingerprint().unwrap(),
                host.kernel_version.unwrap()
            );
            let wsl = guest(&layout, Ok(marker.clone()));
            let result = provision(&layout, &host, false, SHARE).unwrap();
            manifest
                .verify_provisioned(&result, "workload_kernel", host.kernel_version.unwrap())
                .unwrap();
            assert_eq!(wsl.calls().len(), 1);
            assert_eq!(fs::read(&disk).unwrap(), b"keep distribution");
            assert!(!layout.managed_home.join(manifest::FILE_NAME).exists());
            drop(wsl);
            manifest.save(&layout.managed_home).unwrap();
            let untouched =
                fake::answer(|line| panic!("the migrated guest must be preserved: {line}"));
            assert_eq!(provision(&layout, &host, false, SHARE).unwrap(), marker);
            assert!(untouched.calls().is_empty());
            assert_eq!(fs::read(&disk).unwrap(), b"keep distribution");
            drop(untouched);
            super::super::super::managed_home::clear_installation_state(&layout.managed_home)
                .unwrap();
            assert!(!layout.provision_marker().exists());
            assert!(!layout.provision().exists());
            assert!(!layout.managed_home.join(manifest::FILE_NAME).exists());
            let wsl = guest(&layout, Ok(marker.clone()));
            assert_eq!(provision(&layout, &host, true, SHARE).unwrap(), marker);
            assert_eq!(wsl.calls().len(), 1);
            assert_eq!(fs::read(&disk).unwrap(), b"keep distribution");
        }
    }

    #[cfg(unix)]
    #[test]
    fn toml_provisioning_removes_old_development_overrides_and_preserves_user_configuration() {
        let manifest = Manifest::bundled().unwrap();
        let host = host_from_manifest(&manifest, "x86_64").unwrap();
        let script = guest_provision_script(&host, SHARE);
        let cleanup = script
            .split_once("\nphase apt-install\n")
            .unwrap()
            .1
            .split_once("\napt-get install ")
            .unwrap()
            .0;
        let cleanup = cleanup.replace("/etc/systemd/system", "${FIRECRAB_TEST_UNIT_ROOT}");
        let root = tempfile::tempdir().unwrap();
        for unit in ["firecrab-api", "firecrab-helper", "firecrab-net-helper"] {
            let directory = root.path().join(format!("{unit}.service.d"));
            fs::create_dir(&directory).unwrap();
            fs::write(
                directory.join("90-firecrab-dev.conf"),
                b"old development binary",
            )
            .unwrap();
            fs::write(directory.join("10-user.conf"), b"keep user config").unwrap();
        }
        let output = std::process::Command::new("bash")
            .args(["-c", &format!("set -eu\nsystemctl() {{ :; }}\n{cleanup}")])
            .env("FIRECRAB_TEST_UNIT_ROOT", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for unit in ["firecrab-api", "firecrab-helper", "firecrab-net-helper"] {
            let directory = root.path().join(format!("{unit}.service.d"));
            assert!(!directory.join("90-firecrab-dev.conf").exists());
            assert_eq!(
                fs::read(directory.join("10-user.conf")).unwrap(),
                b"keep user config"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn toml_guest_scripts_are_valid_bash_and_select_debian_not_wsl_kernels() {
        let manifest = Manifest::bundled().unwrap();
        for architecture in ["x86_64", "aarch64"] {
            let host = host_from_manifest(&manifest, architecture).unwrap();
            let directory = tempfile::tempdir().unwrap();
            for (name, script) in [
                ("provision.sh", guest_provision_script(&host, SHARE)),
                ("nested.sh", nested_firecracker_e2e_script(&host)),
            ] {
                let path = directory.path().join(name);
                fs::write(&path, &script).unwrap();
                assert!(
                    std::process::Command::new("bash")
                        .arg("-n")
                        .arg(path)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let script = nested_firecracker_e2e_script(&host);
            let selection = script
                .split_once("\nvmlinuz=")
                .unwrap()
                .1
                .split_once("\ntest -f")
                .unwrap()
                .0;
            let output = std::process::Command::new("bash")
                .args(["-c", &format!("vmlinuz={selection}\nprintf '%s' \"$kver\"")])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                host.kernel_version.unwrap()
            );
        }
    }

    #[test]
    fn the_guest_script_mirrors_the_macos_gates() {
        let script = guest_provision_script(&X86_64, "/mnt/c/Users/it's me/micromanager");
        assert!(script.contains("share='/mnt/c/Users/it'\\''s me/micromanager'"));
        assert!(script.contains("e2fsprogs fakeroot "));
        assert!(script.contains("linux-image-amd64"));
        assert!(script.contains("firecracker-v1.17.0-x86_64.tgz"));
        assert!(script.contains("install-firecrab-v0.2.2.sh\" --no-deps"));
        assert!(script.contains("nested-firecracker-e2e.sh"));
        assert!(script.contains("nested_firecracker=passed"));
        assert!(script.contains(&format!("schema={PROVISION_SCHEMA}")));
        assert!(
            !script.contains("openssh-server"),
            "Windows needs no SSH tunnel"
        );
        assert!(!script.contains('\r'));

        let arm = guest_provision_script(&ARM64, "/mnt/c/x/micromanager");
        assert!(arm.contains("linux-image-arm64"));
        assert!(arm.contains("firecracker-v1.17.0-aarch64.tgz"));
    }

    #[test]
    fn the_nested_gate_uses_each_architectures_kernel_format() {
        let x86 = nested_firecracker_e2e_script(&X86_64);
        assert!(x86.contains("/usr/local/lib/firecrab/extract-vmlinux"));
        assert!(x86.contains("/bin/busybox reboot -f"));
        assert!(x86.contains("pci=off"));
        assert!(x86.contains("FIRECRAB_NESTED_WORKLOAD_OK"));
        assert!(x86.contains("</dev/null"));
        assert!(x86.contains("test \"$rc\" -eq 0"));

        let arm = nested_firecracker_e2e_script(&ARM64);
        assert!(arm.contains("/usr/local/lib/firecrab/extract-arm64-image"));
        assert!(arm.contains("/bin/busybox poweroff -f"));
        assert!(arm.contains("keep_bootcon"));
    }

    #[test]
    fn scripts_are_written_with_unix_line_endings() {
        let (_directory, layout) = layout();
        let path = layout.provision().join("guest-provision.sh");
        write_script(&path, &guest_provision_script(&X86_64, "/mnt/c/x")).expect("written");
        let bytes = fs::read(&path).expect("readable");
        assert!(!bytes.contains(&b'\r'));
        remove_if_present(&path).expect("removed");
        remove_if_present(&path).expect("removing twice is fine");
    }
}
