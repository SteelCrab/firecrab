//! In-process packer: stream a registered template into `.packages/{alias}.tar.zst`.
//!
//! Members are the registered relative kernel and rootfs paths, the registered
//! initrd path when present, and [`TEMPLATE_SPEC_MEMBER`]. Every member must
//! pass [`crate::image_install::is_safe_archive_member`].

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::image_install::{
    ImageInstallTracker, clear_staged_package_origin, is_safe_archive_member, package_name,
    staged_package_path, write_staged_package_origin,
};
use crate::templates::{TemplateRegistry, TemplateSpec, TemplateVersion};
use firecrab_api_types::PackageOrigin;

/// CamelCase [`TemplateSpec`] carried beside the kernel so custom aliases keep
/// boot args and the initrd flag. Already under `kernel/`, so the member rule
/// does not need an exception.
pub const TEMPLATE_SPEC_MEMBER: &str = "kernel/.firecrab-template.json";

/// Result of publishing `{alias}.tar.zst` into the local package cache.
#[derive(Debug)]
pub struct PackedPackage {
    /// `{alias}.tar.zst`.
    pub package: String,
    /// SHA-256 of the compressed archive bytes.
    pub sha256: String,
}

/// Temporary path used while the archive is still being written.
pub fn building_package_path(image_root: &Path, alias: &str) -> PathBuf {
    image_root
        .join(".packages")
        .join(format!("{}.tar.zst.building", alias))
}

/// Delete a published `{alias}.tar.zst` and its `.origin` sidecar.
///
/// Used when pack has already published and a later catalog insert fails.
pub(crate) fn discard_staged_package(image_root: &Path, alias: &str) {
    let dest = staged_package_path(image_root, alias);
    let _ = fs::remove_file(dest);
    let _ = clear_staged_package_origin(image_root, alias);
}

/// Discards a published archive unless [`Self::keep`] is called after insert.
pub(crate) struct StagedPackageGuard {
    image_root: PathBuf,
    alias: String,
    keep: bool,
}

impl StagedPackageGuard {
    pub(crate) fn after_publish(image_root: &Path, alias: &str) -> Self {
        Self {
            image_root: image_root.to_path_buf(),
            alias: alias.to_owned(),
            keep: false,
        }
    }

    pub(crate) fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for StagedPackageGuard {
    fn drop(&mut self) {
        if !self.keep {
            discard_staged_package(&self.image_root, &self.alias);
        }
    }
}

/// Pack the installed template for `alias` using the register request's version.
pub async fn pack_registered_template(
    tracker: &ImageInstallTracker,
    templates: &TemplateRegistry,
    alias: &str,
    request_version: &str,
) -> Result<PackedPackage, String> {
    let Some(template) = templates.resolve_alias(alias) else {
        return Err("template is not installed".to_owned());
    };
    let spec = spec_from_registered(&template, request_version);
    let image_root = templates.image_root_path().to_path_buf();
    let tracker = tracker.clone();
    tokio::task::spawn_blocking(move || pack_spec_blocking(&tracker, &image_root, &spec))
        .await
        .map_err(|error| format!("pack task panicked: {error}"))?
}

/// Read the [`TemplateSpec`] stored at [`TEMPLATE_SPEC_MEMBER`].
#[cfg(test)]
pub fn read_packed_template_spec(archive: &Path) -> Result<TemplateSpec, String> {
    let file =
        File::open(archive).map_err(|error| format!("open {}: {error}", archive.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|error| format!("zstd decoder {}: {error}", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);
    for entry in tar
        .entries()
        .map_err(|error| format!("tar entries: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("tar entry: {error}"))?;
        let name = entry
            .path()
            .map_err(|error| format!("tar member path: {error}"))?;
        let name = name.to_string_lossy().replace('\\', "/");
        if name == TEMPLATE_SPEC_MEMBER {
            return serde_json::from_reader(&mut entry)
                .map_err(|error| format!("parse {TEMPLATE_SPEC_MEMBER}: {error}"));
        }
    }
    Err(format!(
        "archive missing required member `{TEMPLATE_SPEC_MEMBER}`"
    ))
}

/// List archive member paths in the order they were written.
#[cfg(test)]
pub fn list_packed_members(archive: &Path) -> Result<Vec<String>, String> {
    let file =
        File::open(archive).map_err(|error| format!("open {}: {error}", archive.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|error| format!("zstd decoder {}: {error}", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);
    let mut members = Vec::new();
    for entry in tar
        .entries()
        .map_err(|error| format!("tar entries: {error}"))?
    {
        let entry = entry.map_err(|error| format!("tar entry: {error}"))?;
        let name = entry
            .path()
            .map_err(|error| format!("tar member path: {error}"))?;
        members.push(name.to_string_lossy().replace('\\', "/"));
    }
    Ok(members)
}

fn spec_from_registered(template: &TemplateVersion, request_version: &str) -> TemplateSpec {
    TemplateSpec {
        alias: template.name.clone(),
        version: request_version.to_owned(),
        kernel: template.kernel.relative_path().to_path_buf(),
        initrd: template
            .initrd
            .as_ref()
            .map(|artifact| artifact.relative_path().to_path_buf()),
        rootfs: template.rootfs.relative_path().to_path_buf(),
        boot_args: template.boot_args.clone(),
    }
}

fn pack_spec_blocking(
    tracker: &ImageInstallTracker,
    image_root: &Path,
    spec: &TemplateSpec,
) -> Result<PackedPackage, String> {
    let kernel = archive_member_name(&spec.kernel, "kernel/")?;
    let rootfs = archive_member_name(&spec.rootfs, "rootfs/")?;
    let initrd = spec
        .initrd
        .as_ref()
        .map(|path| archive_member_name(path, "kernel/"))
        .transpose()?;
    if !is_safe_archive_member(TEMPLATE_SPEC_MEMBER) {
        return Err(format!(
            "refusing archive member `{TEMPLATE_SPEC_MEMBER}` (only kernel/ and rootfs/ relative paths allowed)"
        ));
    }

    tracker.append_log(
        &spec.alias,
        format!("[packer:source] packing registered template {}", spec.alias),
    );
    tracker.append_log(&spec.alias, "[packer:source] complete");

    let dest = staged_package_path(image_root, &spec.alias);
    let building_path = building_package_path(image_root, &spec.alias);
    let (building, file) = BuildingFile::create(building_path)?;
    let sha256 = write_archive(
        tracker,
        image_root,
        spec,
        &kernel,
        initrd.as_deref(),
        &rootfs,
        file,
    )?;
    building.publish(&dest)?;
    if let Err(error) =
        write_staged_package_origin(image_root, &spec.alias, PackageOrigin::MicroRegistry)
    {
        let _ = fs::remove_file(&dest);
        return Err(format!("publish package origin: {error}"));
    }
    tracker.append_log(
        &spec.alias,
        format!("[packer:package] published {}", package_name(&spec.alias)),
    );
    tracker.append_log(&spec.alias, "[packer:package] complete");
    Ok(PackedPackage {
        package: package_name(&spec.alias),
        sha256,
    })
}

fn write_archive(
    tracker: &ImageInstallTracker,
    image_root: &Path,
    spec: &TemplateSpec,
    kernel: &str,
    initrd: Option<&str>,
    rootfs: &str,
    file: File,
) -> Result<String, String> {
    let hasher = HashingWriter::new(file);
    let encoder = zstd::stream::write::Encoder::new(hasher, 0)
        .map_err(|error| format!("zstd encoder: {error}"))?;
    let mut builder = tar::Builder::new(encoder);

    append_template_spec(&mut builder, spec)?;
    tracker.append_log(&spec.alias, format!("[packer:kernel] packing {kernel}"));
    append_regular_file(&mut builder, image_root, kernel)?;
    if let Some(initrd) = initrd {
        tracker.append_log(&spec.alias, format!("[packer:kernel] packing {initrd}"));
        append_regular_file(&mut builder, image_root, initrd)?;
    }
    tracker.append_log(&spec.alias, "[packer:kernel] complete");
    tracker.append_log(&spec.alias, format!("[packer:rootfs] streaming {rootfs}"));
    append_regular_file(&mut builder, image_root, rootfs)?;
    tracker.append_log(&spec.alias, "[packer:rootfs] complete");

    let encoder = builder
        .into_inner()
        .map_err(|error| format!("finish tar: {error}"))?;
    let hasher = encoder
        .finish()
        .map_err(|error| format!("finish zstd: {error}"))?;
    let (file, digest) = hasher.finalize();
    file.sync_all()
        .map_err(|error| format!("sync package: {error}"))?;
    Ok(digest)
}

fn append_template_spec<W: Write>(
    builder: &mut tar::Builder<W>,
    spec: &TemplateSpec,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(spec)
        .map_err(|error| format!("serialize {TEMPLATE_SPEC_MEMBER}: {error}"))?;
    bytes.push(b'\n');
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, TEMPLATE_SPEC_MEMBER, bytes.as_slice())
        .map_err(|error| format!("append {TEMPLATE_SPEC_MEMBER}: {error}"))
}

fn append_regular_file<W: Write>(
    builder: &mut tar::Builder<W>,
    image_root: &Path,
    member: &str,
) -> Result<(), String> {
    let path = image_root.join(member);
    let mut file =
        File::open(&path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("stat {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("template artifact is not a regular file: {member}"));
    }
    builder
        .append_file(member, &mut file)
        .map_err(|error| format!("append {member}: {error}"))
}

fn archive_member_name(path: &Path, required_prefix: &str) -> Result<String, String> {
    let name = path
        .to_str()
        .ok_or_else(|| "template artifact path must be utf-8".to_owned())?
        .replace('\\', "/");
    if !is_safe_archive_member(&name) {
        return Err(format!(
            "refusing archive member `{name}` (only kernel/ and rootfs/ relative paths allowed)"
        ));
    }
    if !name.starts_with(required_prefix) {
        return Err(format!(
            "registered path `{name}` must be under {required_prefix}"
        ));
    }
    Ok(name)
}

struct BuildingFile {
    path: PathBuf,
    persist: bool,
}

impl BuildingFile {
    fn create(path: PathBuf) -> Result<(Self, File), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
        }
        let _ = fs::remove_file(&path);
        let file =
            File::create(&path).map_err(|error| format!("create {}: {error}", path.display()))?;
        Ok((
            Self {
                path,
                persist: false,
            },
            file,
        ))
    }

    fn publish(mut self, dest: &Path) -> Result<(), String> {
        fs::rename(&self.path, dest)
            .map_err(|error| format!("publish {}: {error}", dest.display()))?;
        self.persist = true;
        Ok(())
    }
}

impl Drop for BuildingFile {
    fn drop(&mut self) {
        if !self.persist {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn finalize(self) -> (W, String) {
        (self.inner, format!("{:x}", self.hasher.finalize()))
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_install::{run_image_install, staged_package_exists};
    use crate::templates::TemplateRegistry;
    use firecrab_api_types::ImageInstallStatus;
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use tempfile::tempdir;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn nginx_spec(version: &str) -> TemplateSpec {
        TemplateSpec {
            alias: "nginx-1.27".to_owned(),
            version: version.to_owned(),
            kernel: PathBuf::from("kernel/vmlinux-ubuntu-26.04-x86_64"),
            initrd: None,
            rootfs: PathBuf::from("rootfs/nginx-1.27.ext4"),
            boot_args: "console=ttyS0 root=/dev/vda rw".to_owned(),
        }
    }

    fn alpine_spec() -> TemplateSpec {
        TemplateSpec {
            alias: "alpine-3.24.1".to_owned(),
            version: "5".to_owned(),
            kernel: PathBuf::from("kernel/vmlinux-alpine-virt-x86_64"),
            initrd: Some(PathBuf::from("kernel/initramfs-alpine-virt-x86_64")),
            rootfs: PathBuf::from("rootfs/alpine-rootfs-3.24.1-x86_64.ext4"),
            boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rootfstype=ext4 rw"
                .to_owned(),
        }
    }

    fn register_layout(root: &Path, spec: &TemplateSpec) -> TemplateRegistry {
        write_file(&root.join(&spec.kernel), b"kernel-bytes");
        if let Some(initrd) = &spec.initrd {
            write_file(&root.join(initrd), b"initrd-bytes");
        }
        write_file(&root.join(&spec.rootfs), b"rootfs-bytes");
        TemplateRegistry::from_specs(root, [spec.clone()]).unwrap()
    }

    fn file_sha256(path: &Path) -> String {
        let mut file = File::open(path).unwrap();
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        format!("{:x}", hasher.finalize())
    }

    #[test]
    fn template_spec_member_is_safe_and_top_level_manifest_is_not() {
        assert!(is_safe_archive_member(TEMPLATE_SPEC_MEMBER));
        assert!(is_safe_archive_member("kernel/vmlinux-ubuntu-26.04-x86_64"));
        assert!(is_safe_archive_member("rootfs/nginx-1.27.ext4"));
        assert!(!is_safe_archive_member("manifest.json"));
        assert!(!is_safe_archive_member("kernel/../x"));
        assert!(!is_safe_archive_member("/kernel/x"));
        assert!(!is_safe_archive_member("nginx-1.27.ext4"));
    }

    #[tokio::test]
    async fn packed_members_all_pass_is_safe_archive_member() {
        let directory = tempdir().unwrap();
        let spec = nginx_spec("1");
        let templates = register_layout(directory.path(), &spec);
        let tracker = ImageInstallTracker::disabled();
        let packed = pack_registered_template(&tracker, &templates, &spec.alias, &spec.version)
            .await
            .unwrap();

        let archive = staged_package_path(templates.image_root_path(), &spec.alias);
        let members = list_packed_members(&archive).unwrap();
        assert!(!members.is_empty());
        for member in &members {
            assert!(
                is_safe_archive_member(member),
                "member `{member}` must pass is_safe_archive_member"
            );
            assert_ne!(member, "manifest.json");
            assert!(!member.starts_with("manifest.json"));
        }
        assert!(members.iter().any(|member| member == TEMPLATE_SPEC_MEMBER));
        assert!(
            members
                .iter()
                .any(|member| member == "kernel/vmlinux-ubuntu-26.04-x86_64")
        );
        assert!(
            members
                .iter()
                .any(|member| member == "rootfs/nginx-1.27.ext4")
        );
        assert_eq!(packed.package, "nginx-1.27.tar.zst");
    }

    #[tokio::test]
    async fn packs_oci_shaped_archive_without_initrd() {
        let directory = tempdir().unwrap();
        let spec = nginx_spec("1");
        let templates = register_layout(directory.path(), &spec);
        let tracker = ImageInstallTracker::disabled();
        pack_registered_template(&tracker, &templates, &spec.alias, &spec.version)
            .await
            .unwrap();

        let archive = staged_package_path(templates.image_root_path(), &spec.alias);
        let members = list_packed_members(&archive).unwrap();
        assert_eq!(
            members,
            [
                TEMPLATE_SPEC_MEMBER,
                "kernel/vmlinux-ubuntu-26.04-x86_64",
                "rootfs/nginx-1.27.ext4",
            ]
        );

        let packed_spec = read_packed_template_spec(&archive).unwrap();
        assert_eq!(packed_spec.alias, "nginx-1.27");
        assert_eq!(packed_spec.version, "1");
        assert_eq!(
            packed_spec.kernel,
            PathBuf::from("kernel/vmlinux-ubuntu-26.04-x86_64")
        );
        assert_eq!(packed_spec.initrd, None);
        assert_eq!(packed_spec.rootfs, PathBuf::from("rootfs/nginx-1.27.ext4"));
        assert_eq!(packed_spec.boot_args, spec.boot_args);

        let raw = raw_template_spec_json(&archive);
        let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert!(value.get("bootArgs").is_some());
        assert!(value.get("initrd").unwrap().is_null());
    }

    #[tokio::test]
    async fn packs_alpine_shaped_archive_with_initrd() {
        let directory = tempdir().unwrap();
        let spec = alpine_spec();
        let templates = register_layout(directory.path(), &spec);
        let tracker = ImageInstallTracker::disabled();
        pack_registered_template(&tracker, &templates, &spec.alias, &spec.version)
            .await
            .unwrap();

        let archive = staged_package_path(templates.image_root_path(), &spec.alias);
        let members = list_packed_members(&archive).unwrap();
        assert_eq!(
            members,
            [
                TEMPLATE_SPEC_MEMBER,
                "kernel/vmlinux-alpine-virt-x86_64",
                "kernel/initramfs-alpine-virt-x86_64",
                "rootfs/alpine-rootfs-3.24.1-x86_64.ext4",
            ]
        );
        for member in &members {
            assert!(is_safe_archive_member(member), "{member}");
        }

        let packed_spec = read_packed_template_spec(&archive).unwrap();
        assert_eq!(
            packed_spec.initrd,
            Some(PathBuf::from("kernel/initramfs-alpine-virt-x86_64"))
        );
        assert_eq!(packed_spec.boot_args, spec.boot_args);
        assert_eq!(
            packed_spec.kernel,
            PathBuf::from("kernel/vmlinux-alpine-virt-x86_64")
        );
        assert_eq!(
            packed_spec.rootfs,
            PathBuf::from("rootfs/alpine-rootfs-3.24.1-x86_64.ext4")
        );
    }

    #[tokio::test]
    async fn round_trip_custom_alias_installs_through_existing_path() {
        let source = tempdir().unwrap();
        let spec = nginx_spec("1");
        let templates = register_layout(source.path(), &spec);
        let tracker = ImageInstallTracker::disabled();
        pack_registered_template(&tracker, &templates, &spec.alias, "1")
            .await
            .unwrap();

        let archive = staged_package_path(templates.image_root_path(), &spec.alias);
        let packed_spec = read_packed_template_spec(&archive).unwrap();
        assert_eq!(packed_spec.boot_args, spec.boot_args);
        assert_eq!(packed_spec.version, "1");

        let dest = tempdir().unwrap();
        let dest_templates = TemplateRegistry::from_specs(dest.path(), std::iter::empty()).unwrap();
        let staged = staged_package_path(dest_templates.image_root_path(), &packed_spec.alias);
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::copy(&archive, &staged).unwrap();

        let install_tracker = ImageInstallTracker::disabled();
        install_tracker.begin(&packed_spec.alias).unwrap();
        run_image_install(
            install_tracker.clone(),
            dest_templates.clone(),
            packed_spec.clone(),
        )
        .await;
        let snapshot = install_tracker.snapshot(&packed_spec.alias);
        assert_eq!(
            snapshot.status,
            ImageInstallStatus::Succeeded,
            "log:\n{}",
            snapshot.log
        );

        let installed = dest_templates
            .resolve_alias(&packed_spec.alias)
            .expect("round-trip must register the alias");
        assert_eq!(installed.boot_args, spec.boot_args);
        assert_eq!(installed.version, "1");
        assert_eq!(
            installed.kernel.relative_path(),
            Path::new("kernel/vmlinux-ubuntu-26.04-x86_64")
        );
        assert!(installed.initrd.is_none());
        assert_eq!(
            installed.rootfs.relative_path(),
            Path::new("rootfs/nginx-1.27.ext4")
        );
    }

    #[tokio::test]
    async fn failure_leaves_no_staged_package_or_building_file() {
        let directory = tempdir().unwrap();
        let spec = nginx_spec("1");
        let templates = register_layout(directory.path(), &spec);
        fs::remove_file(templates.image_root_path().join(&spec.rootfs)).unwrap();

        let tracker = ImageInstallTracker::disabled();
        let error = pack_registered_template(&tracker, &templates, &spec.alias, &spec.version)
            .await
            .unwrap_err();
        assert!(
            error.contains("open") || error.contains("rootfs"),
            "{error}"
        );
        assert!(!staged_package_exists(
            templates.image_root_path(),
            &spec.alias
        ));
        assert!(!staged_package_path(templates.image_root_path(), &spec.alias).exists());
        assert!(!building_package_path(templates.image_root_path(), &spec.alias).exists());
    }

    #[tokio::test]
    async fn unsafe_registered_rootfs_is_refused_without_archive() {
        let directory = tempdir().unwrap();
        let spec = TemplateSpec {
            alias: "nginx-1.27".to_owned(),
            version: "1".to_owned(),
            kernel: PathBuf::from("kernel/vmlinux"),
            initrd: None,
            rootfs: PathBuf::from("nginx-1.27.ext4"),
            boot_args: "console=ttyS0 root=/dev/vda rw".to_owned(),
        };
        write_file(&directory.path().join(&spec.kernel), b"kernel-bytes");
        write_file(&directory.path().join(&spec.rootfs), b"rootfs-bytes");
        let templates = TemplateRegistry::from_specs(directory.path(), [spec.clone()]).unwrap();

        let tracker = ImageInstallTracker::disabled();
        let error = pack_registered_template(&tracker, &templates, &spec.alias, &spec.version)
            .await
            .unwrap_err();
        assert!(
            error.contains("nginx-1.27.ext4"),
            "unsafe rootfs must be named in the error: {error}"
        );
        assert!(!staged_package_exists(
            templates.image_root_path(),
            &spec.alias
        ));
        assert!(!building_package_path(templates.image_root_path(), &spec.alias).exists());
    }

    #[tokio::test]
    async fn packer_digest_matches_published_archive_sha256() {
        let directory = tempdir().unwrap();
        let spec = nginx_spec("1");
        let templates = register_layout(directory.path(), &spec);
        let tracker = ImageInstallTracker::disabled();
        let packed = pack_registered_template(&tracker, &templates, &spec.alias, &spec.version)
            .await
            .unwrap();
        let archive = staged_package_path(templates.image_root_path(), &spec.alias);
        assert_eq!(packed.sha256, file_sha256(&archive));
        assert_eq!(packed.sha256.len(), 64);
    }

    fn raw_template_spec_json(archive: &Path) -> Vec<u8> {
        let file = File::open(archive).unwrap();
        let decoder = zstd::stream::read::Decoder::new(file).unwrap();
        let mut tar = tar::Archive::new(decoder);
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().replace('\\', "/");
            if name == TEMPLATE_SPEC_MEMBER {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                return bytes;
            }
        }
        panic!("missing {TEMPLATE_SPEC_MEMBER}");
    }
}
