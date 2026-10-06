//! Imports the managed distribution and provisions Firecrab inside it.
//!
//! The guest steps mirror the macOS provisioner: the same pinned Firecracker,
//! the latest Firecrab release, the same `install.sh --no-deps`, and the same
//! nested Firecracker gate before anything is reported as installed.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::artifact::{self, ArtifactSpec, HashAlgorithm};
use super::super::release::{self, Release};
use super::lifecycle::Layout;
use super::wsl::{self, DISTRO_NAME};

pub const DEBIAN_ROOTFS_VERSION: &str = "1.26.0.0";
pub const FIRECRACKER_VERSION: &str = "v1.17.0";

/// Bumped whenever the guest steps change, so an older marker forces a re-run.
const PROVISION_SCHEMA: u32 = 1;

/// Everything that differs between x86_64 and ARM64 Windows hosts.
pub struct Host {
    pub architecture: &'static str,
    /// Suffix of Debian's `linux-image-*` metapackage.
    debian_architecture: &'static str,
    /// Debian's rootfs and Firecracker stay pinned; Firecrab comes from the latest release.
    pinned: [ArtifactSpec; 2],
    /// The host bundle's name in a Firecrab release, which it keeps once downloaded.
    firecrab_bundle: &'static str,
    firecrab_label: &'static str,
    /// `install.sh` places this helper; it turns Debian's vmlinuz into what
    /// Firecracker boots on this architecture.
    kernel_extractor: &'static str,
    boot_args: &'static str,
    /// x86_64 Firecracker exits on a guest reboot; ARM64 also on power-off.
    workload_exit: &'static str,
}

/// Microsoft's WSL manifest points `wsl --install -d Debian` at these Debian
/// builds, so the managed distribution is the same image users already get.
const X86_64: Host = Host {
    architecture: "x86_64",
    debian_architecture: "amd64",
    pinned: [
        ArtifactSpec {
            label: "Debian 13 WSL rootfs",
            filename: Cow::Borrowed("Debian_WSL_AMD64_v1.26.0.0.wsl"),
            url: Cow::Borrowed(concat!(
                "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/",
                "Debian_WSL_AMD64_v1.26.0.0.wsl"
            )),
            algorithm: HashAlgorithm::Sha256,
            digest: Cow::Borrowed(
                "5ec7dc68216e75d1d4d4761474e99d8461a98d316537110314b137122a879e0f",
            ),
        },
        ArtifactSpec {
            label: "Firecracker x86_64 release",
            filename: Cow::Borrowed("firecracker-v1.17.0-x86_64.tgz"),
            url: Cow::Borrowed(
                "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz",
            ),
            algorithm: HashAlgorithm::Sha256,
            digest: Cow::Borrowed(
                "06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558",
            ),
        },
    ],
    firecrab_bundle: "firecrab-host-x86_64-gnu.tar.gz",
    firecrab_label: "Firecrab x86_64 GNU host bundle",
    kernel_extractor: "extract-vmlinux",
    boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/init",
    workload_exit: "reboot -f",
};

const ARM64: Host = Host {
    architecture: "aarch64",
    debian_architecture: "arm64",
    pinned: [
        ArtifactSpec {
            label: "Debian 13 WSL rootfs",
            filename: Cow::Borrowed("Debian_WSL_ARM64_v1.26.0.0.wsl"),
            url: Cow::Borrowed(concat!(
                "https://salsa.debian.org/debian/WSL/-/jobs/9606244/artifacts/raw/",
                "Debian_WSL_ARM64_v1.26.0.0.wsl"
            )),
            algorithm: HashAlgorithm::Sha256,
            digest: Cow::Borrowed(
                "09120df4fadc36fb2a0f7298e197785e6f599f12aaf95d43cba07a8ac7fb316b",
            ),
        },
        ArtifactSpec {
            label: "Firecracker ARM64 release",
            filename: Cow::Borrowed("firecracker-v1.17.0-aarch64.tgz"),
            url: Cow::Borrowed(
                "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-aarch64.tgz",
            ),
            algorithm: HashAlgorithm::Sha256,
            digest: Cow::Borrowed(
                "e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256",
            ),
        },
    ],
    firecrab_bundle: "firecrab-host-aarch64-gnu.tar.gz",
    firecrab_label: "Firecrab ARM64 GNU host bundle",
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
    Release(#[from] release::Error),
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

/// The pinned artifacts and Firecrab bundle for the architecture this CLI was built for.
pub fn host() -> Result<&'static Host, Error> {
    host_for(std::env::consts::ARCH).ok_or(Error::UnsupportedArchitecture(std::env::consts::ARCH))
}

fn host_for(architecture: &str) -> Option<&'static Host> {
    match architecture {
        "x86_64" => Some(&X86_64),
        "aarch64" => Some(&ARM64),
        _ => None,
    }
}

pub struct Downloaded {
    pub debian_rootfs: PathBuf,
    pub firecracker: PathBuf,
    pub firecrab_host: PathBuf,
    pub installer: PathBuf,
    /// The Firecrab release the host bundle and installer belong to.
    pub firecrab_version: String,
}

pub fn download_all(host: &Host, directory: &Path, assume_yes: bool) -> Result<Downloaded, Error> {
    let release = Release::latest()?;
    let [debian, firecracker] = host.pinned.clone();
    let bundle = release.artifact(
        host.firecrab_label,
        host.firecrab_bundle,
        host.firecrab_bundle,
    )?;
    let installer = release.installer()?;
    let artifacts = [debian, firecracker, bundle, installer];
    artifact::fetch_all(&artifacts, directory, assume_yes)?;
    release.record(directory)?;
    let [debian, firecracker, bundle, installer] = &artifacts;
    Ok(Downloaded {
        debian_rootfs: directory.join(&*debian.filename),
        firecracker: directory.join(&*firecracker.filename),
        firecrab_host: directory.join(&*bundle.filename),
        installer: directory.join(&*installer.filename),
        firecrab_version: release.tag().to_owned(),
    })
}

/// Re-hashes every cached artifact without touching the network. Firecrab's
/// are checked against the digests recorded when they were downloaded.
pub fn validate_downloads(
    host: &Host,
    directory: &Path,
) -> Vec<(&'static str, Result<(), artifact::Error>)> {
    let pinned = host.pinned.iter().map(|spec| {
        let path = directory.join(&*spec.filename);
        (
            spec.label,
            artifact::verify(&path, spec.algorithm, &spec.digest),
        )
    });
    let recorded = [
        (
            host.firecrab_label,
            host.firecrab_bundle,
            host.firecrab_bundle,
        ),
        (
            "Firecrab guest installer",
            release::INSTALLER_ASSET,
            release::INSTALLER_FILE,
        ),
    ]
    .map(|(label, asset, filename)| {
        let result = match release::recorded_digest(directory, asset) {
            Some(digest) => {
                artifact::verify(&directory.join(filename), HashAlgorithm::Sha256, &digest)
            }
            None => Err(artifact::Error::Unrecorded(asset.to_owned())),
        };
        (label, result)
    });
    pinned.chain(recorded).collect()
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
        println!("[PASS] guest: Firecrab provisioning (preserved)");
        return Ok(result);
    }
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
apt-get install -y ca-certificates curl sudo tar findutils xz-utils gzip zstd lz4 lzop \
  nftables dnsmasq dnsmasq-utils iproute2 jq e2fsprogs fakeroot busybox-static socat \
  binutils initramfs-tools linux-image-{debian_architecture}

phase firecracker
tmp=$(mktemp -d)
tar -xzf "$share/downloads/firecracker-{firecracker}-{architecture}.tgz" -C "$tmp"
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
  echo "kernel=$(uname -r)"
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
        debian_architecture = host.debian_architecture,
        architecture = host.architecture,
        firecracker = FIRECRACKER_VERSION,
        installer = release::INSTALLER_FILE,
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
vmlinuz=$(ls -1 /boot/vmlinuz-* | sort -V | tail -n 1)
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
            assert!(host.pinned[0].filename.contains(marker));
            for spec in &host.pinned {
                assert_eq!(spec.algorithm, HashAlgorithm::Sha256, "{}", spec.label);
                assert_eq!(spec.digest.len(), 64, "{} digest length", spec.label);
                assert!(spec.digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(spec.url.starts_with("https://"), "{} scheme", spec.label);
                assert!(spec.url.ends_with(&*spec.filename));
            }
            assert!(host.pinned[0].url.contains("salsa.debian.org/debian/WSL/"));
            assert!(host.pinned[0].url.contains(DEBIAN_ROOTFS_VERSION));
            assert!(host.pinned[1].url.contains(FIRECRACKER_VERSION));
            assert!(host.pinned[1].filename.contains(architecture));
            // Firecrab is not pinned: its version and digests come from the latest release.
            assert!(host.firecrab_bundle.contains(architecture));
        }
        assert!(host_for("x86").is_none());
    }

    #[test]
    fn validation_reports_every_artifact_without_downloading() {
        let (_directory, layout) = layout();
        let results = validate_downloads(&X86_64, &layout.downloads());
        assert_eq!(results.len(), X86_64.pinned.len() + 2);
        assert!(
            results.iter().all(|(_, result)| result.is_err()),
            "nothing is cached"
        );
    }

    #[test]
    fn validation_checks_firecrab_downloads_against_their_recorded_digests() {
        const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let (_directory, layout) = layout();
        let downloads = layout.downloads();
        fs::create_dir_all(&downloads).expect("downloads directory");
        for name in [X86_64.firecrab_bundle, release::INSTALLER_FILE] {
            fs::write(downloads.join(name), b"abc").expect("cached download");
        }
        let bundle = X86_64.firecrab_bundle;
        let recorded = format!(
            "{ABC_SHA256}  ./{bundle}\n{ABC_SHA256}  ./{}\n",
            release::INSTALLER_ASSET
        );

        let firecrab_results = |downloads: &Path| {
            validate_downloads(&X86_64, downloads)
                .into_iter()
                .skip(X86_64.pinned.len())
                .map(|(_, result)| result)
                .collect::<Vec<_>>()
        };
        assert!(
            firecrab_results(&downloads)
                .iter()
                .all(|result| matches!(result, Err(artifact::Error::Unrecorded(_)))),
            "nothing was recorded yet"
        );

        fs::write(downloads.join("SHA256SUMS"), recorded).expect("recorded digests");
        assert!(firecrab_results(&downloads).iter().all(Result::is_ok));

        fs::write(downloads.join(release::INSTALLER_FILE), b"tampered").expect("tampered");
        let results = firecrab_results(&downloads);
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(artifact::Error::Checksum { .. })));
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
    fn the_guest_script_mirrors_the_macos_gates() {
        let script = guest_provision_script(&X86_64, "/mnt/c/Users/it's me/micromanager");
        assert!(script.contains("share='/mnt/c/Users/it'\\''s me/micromanager'"));
        assert!(script.contains("e2fsprogs fakeroot "));
        assert!(script.contains("linux-image-amd64"));
        assert!(script.contains("firecracker-v1.17.0-x86_64.tgz"));
        assert!(script.contains("install-firecrab.sh\" --no-deps"));
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
