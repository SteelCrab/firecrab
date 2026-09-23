use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use super::super::artifact::{self, ArtifactSpec, HashAlgorithm};

pub const DEBIAN_BUILD: &str = "20260914-2601";
pub const DEBIAN_ARCHIVE: &str = "debian-13-generic-arm64-20260914-2601.tar.xz";
pub const FIRECRACKER_VERSION: &str = "v1.17.0";
pub const FIRECRAB_VERSION: &str = "v0.2.2";

const DEBIAN_URL: &str = "https://cloud.debian.org/images/cloud/trixie/20260914-2601/debian-13-generic-arm64-20260914-2601.tar.xz";
const DEBIAN_SHA512: &str = "7bacdeb825f81b7eb5fcd35330ce7160ed60413adfa61f37cff3c4028fdc61dbd444e046c58934cd0494192f53fa2cbcfcca1e8e1d337755b34c6c90dc72f62b";
const FIRECRACKER_URL: &str = "https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-aarch64.tgz";
const FIRECRACKER_SHA256: &str = "e351ebe4f7a16b5873bbd51005d2e6767103cff4d5ebc829df2d3f95a93e2256";
const FIRECRAB_HOST_URL: &str = "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/firecrab-host-aarch64-gnu.tar.gz";
const FIRECRAB_HOST_SHA256: &str =
    "2e995ed27d8c19848baaf38c8bf8a2d5d126ab90776738d91790524a8564e982";
const FIRECRAB_INSTALL_URL: &str =
    "https://github.com/SteelCrab/firecrab/releases/download/v0.2.2/install.sh";
const FIRECRAB_INSTALL_SHA256: &str =
    "af2fb56b92dff1559cdaa449aa025808e0990437c16ccc0bb2b4d558ef50dacb";
const DEBIAN_RAW_BYTES: u64 = 3 * 1024 * 1024 * 1024;
const DATA_DISK_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const PROVISION_SCHEMA: u32 = 4;
const EXTRACT_ARM64_IMAGE: &str =
    include_str!("../../../../scripts/firecracker-menual/extract-arm64-image");

const ARTIFACTS: [ArtifactSpec; 4] = [
    ArtifactSpec {
        label: "Debian 13 ARM64 management image",
        filename: DEBIAN_ARCHIVE,
        url: DEBIAN_URL,
        algorithm: HashAlgorithm::Sha512,
        digest: DEBIAN_SHA512,
    },
    ArtifactSpec {
        label: "Firecracker ARM64 release",
        filename: "firecracker-v1.17.0-aarch64.tgz",
        url: FIRECRACKER_URL,
        algorithm: HashAlgorithm::Sha256,
        digest: FIRECRACKER_SHA256,
    },
    ArtifactSpec {
        label: "Firecrab ARM64 GNU host bundle",
        filename: "firecrab-host-aarch64-gnu.tar.gz",
        url: FIRECRAB_HOST_URL,
        algorithm: HashAlgorithm::Sha256,
        digest: FIRECRAB_HOST_SHA256,
    },
    ArtifactSpec {
        label: "Firecrab guest installer",
        filename: "install-firecrab-v0.2.2.sh",
        url: FIRECRAB_INSTALL_URL,
        algorithm: HashAlgorithm::Sha256,
        digest: FIRECRAB_INSTALL_SHA256,
    },
];

#[derive(Clone, Debug)]
pub struct DownloadedArtifacts {
    pub debian_archive: PathBuf,
    pub firecracker_archive: PathBuf,
    pub firecrab_host_archive: PathBuf,
    pub firecrab_installer: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PreparedLayout {
    pub os_disk: PathBuf,
    pub data_disk: PathBuf,
    pub seed_iso: PathBuf,
    pub efi_variable_store: PathBuf,
    pub provision_marker: PathBuf,
    pub ssh_private_key: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not create artifact directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Artifact(#[from] artifact::Error),
    #[error("could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("command failed while {action}: {detail}")]
    Command {
        action: &'static str,
        detail: String,
    },
    #[error("invalid managed disk {path}: {detail}")]
    InvalidDisk { path: PathBuf, detail: String },
    #[error("guest provisioning failed: {0}")]
    GuestProvision(String),
}

pub fn download_all(managed_home: &Path) -> Result<DownloadedArtifacts, Error> {
    let directory = managed_home.join("downloads");
    artifact::fetch_all(&ARTIFACTS, &directory)?;
    Ok(DownloadedArtifacts {
        debian_archive: directory.join(ARTIFACTS[0].filename),
        firecracker_archive: directory.join(ARTIFACTS[1].filename),
        firecrab_host_archive: directory.join(ARTIFACTS[2].filename),
        firecrab_installer: directory.join(ARTIFACTS[3].filename),
    })
}

pub fn prepare(
    managed_home: &Path,
    artifacts: &DownloadedArtifacts,
) -> Result<PreparedLayout, Error> {
    let system = managed_home.join("system");
    let data = managed_home.join("data");
    let provision = managed_home.join("provision");
    let runtime = managed_home.join("runtime");
    for directory in [&system, &data, &provision, &runtime] {
        fs::create_dir_all(directory).map_err(|source| Error::CreateDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error("protect directory", directory, source))?;
    }
    reset_failed_system(&system, &runtime)?;

    let os_disk = system.join("debian-system.raw");
    prepare_os_disk(&artifacts.debian_archive, &os_disk, &system)?;
    let root_partuuid = read_linux_root_partuuid(&os_disk)?;
    write_private(
        &runtime.join("root-partuuid"),
        format!("{root_partuuid}\n").as_bytes(),
        0o600,
    )?;
    let data_disk = data.join("firecrab-data.raw");
    prepare_sparse_disk(&data_disk, DATA_DISK_BYTES)?;
    let ssh_private_key = prepare_ssh_key(&runtime, &provision)?;
    prepare_guest_payload(&provision)?;
    let seed_iso = runtime.join("cloud-init-seed.iso");
    prepare_seed_iso(&runtime, &seed_iso)?;

    Ok(PreparedLayout {
        os_disk,
        data_disk,
        seed_iso,
        efi_variable_store: runtime.join("efi-variable-store"),
        provision_marker: runtime.join("provisioned"),
        ssh_private_key,
    })
}

pub fn guest_result(prepared: &PreparedLayout) -> Result<Option<String>, Error> {
    let failed = prepared.provision_marker.with_file_name("provision.failed");
    if failed.is_file() {
        let detail = fs::read_to_string(&failed)
            .map_err(|source| io_error("read provisioning failure", &failed, source))?;
        return Err(Error::GuestProvision(format!(
            "guest first boot exited with {}",
            detail.trim()
        )));
    }
    if !prepared.provision_marker.is_file() {
        return Ok(None);
    }
    let result = fs::read_to_string(&prepared.provision_marker).map_err(|source| {
        io_error(
            "read provisioning marker",
            &prepared.provision_marker,
            source,
        )
    })?;
    let expected_schema = format!("schema={PROVISION_SCHEMA}");
    for required in [
        expected_schema.as_str(),
        "kvm=usable",
        "firecrab_api=active",
        "firecrab_net_helper=active",
        "nested_firecracker=passed",
    ] {
        if !result.lines().any(|line| line == required) {
            return Err(Error::GuestProvision(format!(
                "marker is missing required gate {required}"
            )));
        }
    }
    let managed_home = prepared
        .provision_marker
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| Error::GuestProvision("marker has no managed parent".to_string()))?;
    let kernel = managed_home.join("system/Image");
    let initrd = managed_home.join("system/initrd.img");
    validate_arm64_image(&kernel)?;
    if !initrd.is_file()
        || fs::metadata(&initrd)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            == 0
    {
        return Err(Error::GuestProvision(format!(
            "missing extracted initrd at {}",
            initrd.display()
        )));
    }
    Ok(Some(result))
}

fn validate_arm64_image(path: &Path) -> Result<(), Error> {
    let mut file = File::open(path).map_err(|source| io_error("open ARM64 Image", path, source))?;
    file.seek(SeekFrom::Start(56))
        .map_err(|source| io_error("seek ARM64 Image", path, source))?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)
        .map_err(|source| io_error("read ARM64 Image magic", path, source))?;
    if &magic == b"ARMd" {
        Ok(())
    } else {
        Err(Error::GuestProvision(format!(
            "{} is not a raw ARM64 Image",
            path.display()
        )))
    }
}

fn reset_failed_system(system: &Path, runtime: &Path) -> Result<(), Error> {
    let failed = runtime.join("provision.failed");
    let provisioned = runtime.join("provisioned");
    let outdated = provisioned.is_file()
        && !fs::read_to_string(&provisioned)
            .unwrap_or_default()
            .lines()
            .any(|line| line == format!("schema={PROVISION_SCHEMA}"));
    if !failed.is_file() && !outdated {
        return Ok(());
    }
    println!(
        "[RESET] {} Debian system disk; persistent data is preserved",
        if outdated { "outdated" } else { "failed" }
    );
    for path in [
        system.join("debian-system.raw"),
        system.join("Image"),
        system.join("Image.partial"),
        system.join("initrd.img"),
        runtime.join("efi-variable-store"),
        runtime.join("cloud-init-seed.iso"),
        runtime.join("root-partuuid"),
        runtime.join("provisioned"),
        runtime.join("provision.failed"),
        runtime.join("provision.phase"),
        runtime.join("guest-provision.log"),
        runtime.join("manager-ready"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("reset failed system", &path, source)),
        }
    }
    Ok(())
}

fn prepare_os_disk(archive: &Path, destination: &Path, system: &Path) -> Result<(), Error> {
    if destination.is_file() {
        validate_os_disk(destination)?;
        println!("[PASS] disk: Debian OS disk (preserved)");
        return Ok(());
    }

    println!("[PREPARE] Debian OS disk");
    let staging = tempfile::tempdir_in(system)
        .map_err(|source| io_error("create OS disk staging directory", system, source))?;
    let output = ProcessCommand::new("/usr/bin/tar")
        .args(["-xJf"])
        .arg(archive)
        .args(["-C"])
        .arg(staging.path())
        .arg("disk.raw")
        .output()
        .map_err(|source| io_error("run tar", archive, source))?;
    if !output.status.success() {
        return Err(Error::Command {
            action: "extract Debian OS disk",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let extracted = staging.path().join("disk.raw");
    validate_os_disk(&extracted)?;
    fs::set_permissions(&extracted, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("protect OS disk", &extracted, source))?;
    fs::rename(&extracted, destination)
        .map_err(|source| io_error("publish OS disk", destination, source))?;
    println!("[PASS] disk: Debian OS disk");
    Ok(())
}

fn validate_os_disk(path: &Path) -> Result<(), Error> {
    let metadata =
        fs::metadata(path).map_err(|source| io_error("inspect OS disk", path, source))?;
    if metadata.len() != DEBIAN_RAW_BYTES {
        return Err(Error::InvalidDisk {
            path: path.to_owned(),
            detail: format!("expected {DEBIAN_RAW_BYTES} bytes, got {}", metadata.len()),
        });
    }
    let mut file = File::open(path).map_err(|source| io_error("open OS disk", path, source))?;
    file.seek(SeekFrom::Start(512))
        .map_err(|source| io_error("seek OS disk", path, source))?;
    let mut signature = [0_u8; 8];
    file.read_exact(&mut signature)
        .map_err(|source| io_error("read GPT signature", path, source))?;
    if &signature != b"EFI PART" {
        return Err(Error::InvalidDisk {
            path: path.to_owned(),
            detail: "missing GPT signature".to_string(),
        });
    }
    Ok(())
}

fn read_linux_root_partuuid(path: &Path) -> Result<String, Error> {
    const ROOT_PARTITION_GUIDS: [&str; 2] = [
        "0fc63daf-8483-4772-8e79-3d69d8477de4",
        "b921b045-1df0-41c3-af44-4c6f280d3fae",
    ];
    let mut file = File::open(path).map_err(|source| io_error("open GPT disk", path, source))?;
    file.seek(SeekFrom::Start(512))
        .map_err(|source| io_error("seek GPT header", path, source))?;
    let mut header = [0_u8; 92];
    file.read_exact(&mut header)
        .map_err(|source| io_error("read GPT header", path, source))?;
    if &header[..8] != b"EFI PART" {
        return Err(Error::InvalidDisk {
            path: path.to_owned(),
            detail: "missing GPT header".to_string(),
        });
    }
    let entries_lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let entry_count = u32::from_le_bytes(header[80..84].try_into().unwrap());
    let entry_size = u32::from_le_bytes(header[84..88].try_into().unwrap());
    if entries_lba < 2
        || entry_count == 0
        || entry_count > 4096
        || !(128..=4096).contains(&entry_size)
    {
        return Err(Error::InvalidDisk {
            path: path.to_owned(),
            detail: "invalid GPT partition table geometry".to_string(),
        });
    }
    for index in 0..entry_count {
        let offset = entries_lba * 512 + u64::from(index) * u64::from(entry_size);
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| io_error("seek GPT entry", path, source))?;
        let mut entry = vec![0_u8; entry_size as usize];
        file.read_exact(&mut entry)
            .map_err(|source| io_error("read GPT entry", path, source))?;
        if entry[..16].iter().all(|byte| *byte == 0) {
            continue;
        }
        if ROOT_PARTITION_GUIDS.contains(&format_gpt_guid(&entry[..16]).as_str()) {
            let partuuid = format_gpt_guid(&entry[16..32]);
            if partuuid == "00000000-0000-0000-0000-000000000000" {
                break;
            }
            return Ok(partuuid);
        }
    }
    Err(Error::InvalidDisk {
        path: path.to_owned(),
        detail: "no Linux filesystem GPT partition found".to_string(),
    })
}

fn format_gpt_guid(bytes: &[u8]) -> String {
    debug_assert_eq!(bytes.len(), 16);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[3],
        bytes[2],
        bytes[1],
        bytes[0],
        bytes[5],
        bytes[4],
        bytes[7],
        bytes[6],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn prepare_sparse_disk(path: &Path, size: u64) -> Result<(), Error> {
    if path.is_file() {
        let actual = fs::metadata(path)
            .map_err(|source| io_error("inspect data disk", path, source))?
            .len();
        if actual < size {
            return Err(Error::InvalidDisk {
                path: path.to_owned(),
                detail: format!("expected at least {size} bytes, got {actual}"),
            });
        }
        println!("[PASS] disk: persistent data disk (preserved)");
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| Error::InvalidDisk {
        path: path.to_owned(),
        detail: "has no parent directory".to_string(),
    })?;
    let staged = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| io_error("create data disk", parent, source))?;
    staged
        .as_file()
        .set_len(size)
        .map_err(|source| io_error("size data disk", staged.path(), source))?;
    staged
        .as_file()
        .sync_all()
        .map_err(|source| io_error("sync data disk", staged.path(), source))?;
    staged
        .persist(path)
        .map_err(|error| io_error("publish data disk", path, error.error))?;
    println!("[PASS] disk: persistent data disk");
    Ok(())
}

fn prepare_ssh_key(runtime: &Path, provision: &Path) -> Result<PathBuf, Error> {
    let private_key = runtime.join("manager_ed25519");
    let public_key = runtime.join("manager_ed25519.pub");
    if !private_key.is_file() || !public_key.is_file() {
        let _ = fs::remove_file(&private_key);
        let _ = fs::remove_file(&public_key);
        let output = ProcessCommand::new("/usr/bin/ssh-keygen")
            .args([
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "firecrab-micromanager",
            ])
            .arg("-f")
            .arg(&private_key)
            .output()
            .map_err(|source| io_error("run ssh-keygen", &private_key, source))?;
        if !output.status.success() {
            return Err(Error::Command {
                action: "generate management SSH key",
                detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
    }
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("protect management SSH key", &private_key, source))?;
    let public = fs::read(&public_key)
        .map_err(|source| io_error("read management SSH public key", &public_key, source))?;
    write_private(&provision.join("authorized_key"), &public, 0o600)?;
    Ok(private_key)
}

fn prepare_guest_payload(provision: &Path) -> Result<(), Error> {
    write_private(
        &provision.join("extract-arm64-image"),
        EXTRACT_ARM64_IMAGE.as_bytes(),
        0o700,
    )?;
    write_private(
        &provision.join("guest-provision.sh"),
        guest_provision_script().as_bytes(),
        0o700,
    )?;
    write_private(
        &provision.join("nested-firecracker-e2e.sh"),
        nested_firecracker_e2e_script().as_bytes(),
        0o700,
    )?;
    Ok(())
}

fn guest_provision_script() -> String {
    format!(
        r#"#!/bin/bash
set -Eeuxo pipefail
share=/mnt/firecrab
log=/var/log/firecrab-provision.log
exec >>"$log" 2>&1
mkdir -p "$share"
mountpoint -q "$share" || mount -t virtiofs firecrab "$share"
failed="$share/runtime/provision.failed"
ready="$share/runtime/provisioned"
phase_file="$share/runtime/provision.phase"
rm -f "$failed" "$ready"
phase() {{ echo "$1" >"$phase_file"; sync; }}
finish() {{
  rc=$?
  set +e
  cp -f "$log" "$share/runtime/guest-provision.log"
  if [ "$rc" -ne 0 ]; then echo "$rc" >"$failed"; fi
  sync
  systemctl poweroff --no-block || poweroff -f
}}
trap finish EXIT

phase apt-update
export DEBIAN_FRONTEND=noninteractive
apt-get update
phase apt-install
apt-get install -y ca-certificates curl sudo tar findutils xz-utils gzip zstd lz4 lzop \
  nftables dnsmasq dnsmasq-utils iproute2 jq e2fsprogs fakeroot busybox-static socat openssh-server

phase data-disk
if ! blkid /dev/vdb >/dev/null 2>&1; then
  mkfs.ext4 -F -L firecrab-data /dev/vdb
fi
mkdir -p /var/lib/firecrab
if ! grep -q '^LABEL=firecrab-data ' /etc/fstab; then
  echo 'LABEL=firecrab-data /var/lib/firecrab ext4 defaults,nofail 0 2' >>/etc/fstab
fi
mountpoint -q /var/lib/firecrab || mount /var/lib/firecrab
if ! grep -q '^firecrab /mnt/firecrab ' /etc/fstab; then
  echo 'firecrab /mnt/firecrab virtiofs defaults,nofail 0 0' >>/etc/fstab
fi

phase host-access
install -d -m 0700 /root/.ssh
install -m 0600 "$share/provision/authorized_key" /root/.ssh/authorized_keys
install -d -m 0755 /etc/ssh/sshd_config.d
cat >/etc/ssh/sshd_config.d/60-firecrab.conf <<'SSHCONF'
PermitRootLogin prohibit-password
PasswordAuthentication no
KbdInteractiveAuthentication no
SSHCONF
systemctl enable --now ssh

phase kernel
kernel=/boot/vmlinuz-$(uname -r)
initrd=/boot/initrd.img-$(uname -r)
"$share/provision/extract-arm64-image" "$kernel" >"$share/system/Image.partial"
mv -f "$share/system/Image.partial" "$share/system/Image"
cp -f "$initrd" "$share/system/initrd.img"
chmod 600 "$share/system/Image" "$share/system/initrd.img"

phase firecracker
tmp=$(mktemp -d)
tar -xzf "$share/downloads/firecracker-{firecracker}-aarch64.tgz" -C "$tmp"
fc=$(find "$tmp" -type f -name 'firecracker-{firecracker}-aarch64' -print -quit)
jailer=$(find "$tmp" -type f -name 'jailer-{firecracker}-aarch64' -print -quit)
test -n "$fc"
install -m 0755 "$fc" /usr/local/bin/firecracker
if [ -n "$jailer" ]; then install -m 0755 "$jailer" /usr/local/bin/jailer; fi
rm -rf "$tmp"

phase firecrab
bash "$share/downloads/install-firecrab-{firecrab}.sh" --no-deps
systemctl is-active --quiet firecrab-net-helper
systemctl is-active --quiet firecrab-api

phase nested-firecracker
"$share/provision/nested-firecracker-e2e.sh"

cat >/usr/local/sbin/firecrab-manager-ready <<'READY_SCRIPT'
#!/bin/bash
set -Eeuo pipefail
mkdir -p /mnt/firecrab
mountpoint -q /mnt/firecrab || mount -t virtiofs firecrab /mnt/firecrab
for _ in $(seq 1 60); do
  set -- $(hostname -I)
  [ "$#" -gt 0 ] && break
  sleep 1
done
ip=${{1:-}}
test -n "$ip"
systemctl is-active --quiet firecrab-api
systemctl is-active --quiet firecrab-net-helper
systemctl is-active --quiet ssh
{{
  echo 'schema={schema}'
  echo "ip=$ip"
  echo 'firecrab_api=active'
  echo 'firecrab_net_helper=active'
  echo 'ssh=active'
}} >/mnt/firecrab/runtime/manager-ready
READY_SCRIPT
chmod 0755 /usr/local/sbin/firecrab-manager-ready
cat >/etc/systemd/system/firecrab-manager-ready.service <<'READY_UNIT'
[Unit]
Description=Publish Firecrab management VM readiness to macOS
After=network-online.target firecrab-api.service firecrab-net-helper.service ssh.service
Wants=network-online.target firecrab-api.service firecrab-net-helper.service ssh.service

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/firecrab-manager-ready
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
READY_UNIT
systemctl daemon-reload
systemctl enable --now firecrab-manager-ready.service

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
        firecracker = FIRECRACKER_VERSION,
        firecrab = FIRECRAB_VERSION,
        schema = PROVISION_SCHEMA,
    )
}

fn nested_firecracker_e2e_script() -> &'static str {
    r#"#!/bin/bash
set -Eeuxo pipefail
work=/var/lib/firecrab/e2e-nested
rm -rf "$work"
mkdir -p "$work/rootfs"/{bin,sbin,proc,sys,dev,etc,tmp}
cp /bin/busybox "$work/rootfs/bin/busybox"
ln -s busybox "$work/rootfs/bin/sh"
cat >"$work/rootfs/sbin/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mount -t proc proc /proc || true
/bin/busybox mount -t sysfs sysfs /sys || true
/bin/busybox echo FIRECRAB_NESTED_WORKLOAD_OK
/bin/busybox sync
/bin/busybox poweroff -f
INIT
chmod 0755 "$work/rootfs/sbin/init"
truncate -s 128M "$work/rootfs.ext4"
mkfs.ext4 -q -F -d "$work/rootfs" "$work/rootfs.ext4"
kver=$(uname -r)
/mnt/firecrab/provision/extract-arm64-image "/boot/vmlinuz-$kver" >"$work/Image"
cat >"$work/config.json" <<JSON
{
  "boot-source": {
    "kernel_image_path": "$work/Image",
    "initrd_path": "/boot/initrd.img-$kver",
    "boot_args": "keep_bootcon console=ttyS0 reboot=k panic=1 root=/dev/vda rw init=/sbin/init"
  },
  "drives": [{
    "drive_id": "rootfs",
    "path_on_host": "$work/rootfs.ext4",
    "is_root_device": true,
    "is_read_only": false
  }],
  "machine-config": {
    "vcpu_count": 1,
    "mem_size_mib": 256
  }
}
JSON
rm -f "$work/firecracker.sock" "$work/console.log"
set +e
timeout 60 /usr/local/bin/firecracker \
  --api-sock "$work/firecracker.sock" \
  --config-file "$work/config.json" </dev/null >"$work/console.log" 2>&1
rc=$?
set -e
cat "$work/console.log"
test "$rc" -eq 0
grep -a -q FIRECRAB_NESTED_WORKLOAD_OK "$work/console.log"
echo passed >"$work/result"
"#
}

fn prepare_seed_iso(runtime: &Path, destination: &Path) -> Result<(), Error> {
    if destination.is_file() {
        println!("[PASS] seed: cloud-init ISO (preserved)");
        return Ok(());
    }
    let seed = tempfile::tempdir_in(runtime)
        .map_err(|source| io_error("create cloud-init seed directory", runtime, source))?;
    write_private(
        &seed.path().join("meta-data"),
        b"instance-id: firecrab-micromanager-v1\nlocal-hostname: firecrab-manager\n",
        0o600,
    )?;
    write_private(
        &seed.path().join("network-config"),
        b"version: 2\nethernets:\n  primary:\n    match:\n      name: 'en*'\n    dhcp4: true\n",
        0o600,
    )?;
    let user_data = b"#cloud-config\nssh_pwauth: false\nruncmd:\n  - [ bash, -lc, 'mkdir -p /mnt/firecrab && mount -t virtiofs firecrab /mnt/firecrab && exec bash /mnt/firecrab/provision/guest-provision.sh' ]\n";
    write_private(&seed.path().join("user-data"), user_data, 0o600)?;

    let staged = runtime.join("cloud-init-seed.partial.iso");
    let _ = fs::remove_file(&staged);
    let output = ProcessCommand::new("/usr/bin/hdiutil")
        .args(["makehybrid", "-quiet", "-iso", "-joliet"])
        .args(["-default-volume-name", "cidata", "-o"])
        .arg(&staged)
        .arg(seed.path())
        .output()
        .map_err(|source| io_error("run hdiutil", &staged, source))?;
    if !output.status.success() {
        return Err(Error::Command {
            action: "create cloud-init seed ISO",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("protect cloud-init seed", &staged, source))?;
    fs::rename(&staged, destination)
        .map_err(|source| io_error("publish cloud-init seed", destination, source))?;
    println!("[PASS] seed: cloud-init ISO");
    Ok(())
}

fn write_private(path: &Path, contents: &[u8], mode: u32) -> Result<(), Error> {
    let mut staged =
        tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|source| io_error("create staged file", path, source))?;
    staged
        .write_all(contents)
        .map_err(|source| io_error("write staged file", staged.path(), source))?;
    staged
        .flush()
        .map_err(|source| io_error("flush staged file", staged.path(), source))?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| io_error("set file permissions", staged.path(), source))?;
    staged
        .persist(path)
        .map_err(|error| io_error("publish file", path, error.error))?;
    Ok(())
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        action,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_linux_partition_partuuid_is_derived_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("disk.raw");
        let mut file = File::create(&disk).unwrap();
        file.set_len(4096).unwrap();
        let mut header = [0_u8; 92];
        header[..8].copy_from_slice(b"EFI PART");
        header[72..80].copy_from_slice(&2_u64.to_le_bytes());
        header[80..84].copy_from_slice(&128_u32.to_le_bytes());
        header[84..88].copy_from_slice(&128_u32.to_le_bytes());
        file.seek(SeekFrom::Start(512)).unwrap();
        file.write_all(&header).unwrap();
        let mut entry = [0_u8; 128];
        entry[..16].copy_from_slice(&[
            0x45, 0xb0, 0x21, 0xb9, 0xf0, 0x1d, 0xc3, 0x41, 0xaf, 0x44, 0x4c, 0x6f, 0x28, 0x0d,
            0x3f, 0xae,
        ]);
        entry[16..32].copy_from_slice(&[
            0xed, 0x75, 0x1e, 0x57, 0x38, 0x44, 0xec, 0x40, 0xb6, 0x8e, 0xd5, 0xa9, 0x7d, 0xf8,
            0xf5, 0xae,
        ]);
        file.seek(SeekFrom::Start(1024)).unwrap();
        file.write_all(&entry).unwrap();
        file.sync_all().unwrap();
        assert_eq!(
            read_linux_root_partuuid(&disk).unwrap(),
            "571e75ed-4438-40ec-b68e-d5a97df8f5ae"
        );
    }

    #[test]
    fn guest_scripts_require_ssh_ready_and_nested_firecracker() {
        let provision = guest_provision_script();
        assert!(provision.contains("openssh-server"));
        assert!(
            provision.contains("e2fsprogs fakeroot "),
            "Debian guest apt-get must install fakeroot next to e2fsprogs; OCI import packs ext4 via fakeroot"
        );
        assert!(provision.contains("firecrab-manager-ready.service"));
        assert!(provision.contains("nested-firecracker-e2e.sh"));
        assert!(provision.contains("nested_firecracker=passed"));
        assert!(provision.contains(&format!("schema={PROVISION_SCHEMA}")));

        let nested = nested_firecracker_e2e_script();
        assert!(nested.contains("FIRECRAB_NESTED_WORKLOAD_OK"));
        assert!(nested.contains("</dev/null"));
        assert!(nested.contains("test \"$rc\" -eq 0"));
    }

    #[test]
    fn sparse_data_disk_is_sized_and_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let disk = directory.path().join("data.raw");
        prepare_sparse_disk(&disk, 1024 * 1024).unwrap();
        assert_eq!(fs::metadata(&disk).unwrap().len(), 1024 * 1024);
        fs::write(directory.path().join("sentinel"), b"keep").unwrap();
        prepare_sparse_disk(&disk, 1024 * 1024).unwrap();
        assert_eq!(fs::metadata(&disk).unwrap().len(), 1024 * 1024);
    }

    #[test]
    fn guest_result_requires_every_schema4_gate_and_boot_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let system = directory.path().join("system");
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&system).unwrap();
        let marker = runtime.join("provisioned");
        fs::write(
            &marker,
            format!(
                "schema={PROVISION_SCHEMA}\nkvm=usable\nfirecrab_api=active\nfirecrab_net_helper=active\nnested_firecracker=passed\n"
            ),
        )
        .unwrap();
        let mut kernel = vec![0_u8; 64];
        kernel[56..60].copy_from_slice(b"ARMd");
        fs::write(system.join("Image"), kernel).unwrap();
        fs::write(system.join("initrd.img"), b"initrd").unwrap();
        let prepared = PreparedLayout {
            os_disk: system.join("debian-system.raw"),
            data_disk: directory.path().join("data/firecrab-data.raw"),
            seed_iso: runtime.join("cloud-init-seed.iso"),
            efi_variable_store: runtime.join("efi-variable-store"),
            provision_marker: marker,
            ssh_private_key: runtime.join("manager_ed25519"),
        };
        assert!(guest_result(&prepared).unwrap().is_some());

        fs::write(
            &prepared.provision_marker,
            format!("schema={PROVISION_SCHEMA}\nkvm=usable\n"),
        )
        .unwrap();
        assert!(matches!(
            guest_result(&prepared),
            Err(Error::GuestProvision(_))
        ));
    }

    #[test]
    fn pinned_artifacts_use_immutable_versions_and_valid_digest_lengths() {
        assert!(DEBIAN_URL.contains(DEBIAN_BUILD));
        assert!(FIRECRACKER_URL.contains(FIRECRACKER_VERSION));
        assert!(FIRECRAB_HOST_URL.contains(FIRECRAB_VERSION));
        for artifact in ARTIFACTS {
            assert!(artifact.url.starts_with("https://"));
            let expected_length = match artifact.algorithm {
                HashAlgorithm::Sha256 => 64,
                HashAlgorithm::Sha512 => 128,
            };
            assert_eq!(artifact.digest.len(), expected_length);
            assert!(artifact.digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
}
