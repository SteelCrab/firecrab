//! `GET /api/images` — template catalog for the create form and image UI.

use axum::Json;
use axum::extract::State;
use firecrab_api_types::ImageResponse;

use crate::state::AppState;
use crate::templates::TemplateVersion;

/// Smallest disk (GiB) that can hold `rootfs_bytes`, matching create validation.
fn min_disk_gb_for(rootfs_bytes: u64) -> u16 {
    const GIB: u64 = 1024 * 1024 * 1024;
    rootfs_bytes.div_ceil(GIB).try_into().unwrap_or(u16::MAX)
}

fn image_response(template: &TemplateVersion) -> ImageResponse {
    ImageResponse {
        alias: template.name.clone(),
        version: template.version.clone(),
        kernel_sha256: template.kernel.sha256().to_owned(),
        rootfs_sha256: template.rootfs.sha256().to_owned(),
        initrd_sha256: template
            .initrd
            .as_ref()
            .map(|artifact| artifact.sha256().to_owned()),
        min_disk_gb: min_disk_gb_for(template.rootfs.length()),
        installed: true,
        description: String::new(),
    }
}

/// `GET /api/images`: verified templates currently loadable on this host.
/// Host paths are never included — only alias, version, digests, and disk floor.
pub async fn list_images(State(state): State<AppState>) -> Json<Vec<ImageResponse>> {
    let templates = state.templates.clone();
    let images = tokio::task::spawn_blocking(move || {
        templates
            .list_aliases()
            .into_iter()
            .map(|template| image_response(template.as_ref()))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    Json(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::templates::{TemplateRegistry, TemplateSpec};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    async fn state_from_specs(root: &Path, specs: impl IntoIterator<Item = TemplateSpec>) -> AppState {
        let templates = TemplateRegistry::from_specs(root, specs).expect("registry");
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .expect("state")
    }

    #[tokio::test]
    async fn list_images_returns_empty_when_no_templates_are_registered() {
        let directory = tempdir().unwrap();
        let state = state_from_specs(directory.path(), std::iter::empty()).await;
        let Json(images) = list_images(State(state)).await;
        assert!(images.is_empty());
    }

    #[tokio::test]
    async fn list_images_exposes_alias_version_digests_and_disk_floor() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("kernel/vmlinux"), b"kernel-bytes");
        write_file(&root.join("rootfs/root.ext4"), b"rootfs-content-here");
        let state = state_from_specs(
            root,
            [TemplateSpec {
                alias: "demo".to_owned(),
                version: "demo-v1".to_owned(),
                kernel: Path::new("kernel/vmlinux").to_path_buf(),
                initrd: None,
                rootfs: Path::new("rootfs/root.ext4").to_path_buf(),
                boot_args: "console=ttyS0".to_owned(),
            }],
        )
        .await;

        let Json(images) = list_images(State(state)).await;
        assert_eq!(images.len(), 1);
        let image = &images[0];
        assert_eq!(image.alias, "demo");
        assert_eq!(image.version, "demo-v1");
        assert_eq!(image.kernel_sha256.len(), 64);
        assert_eq!(image.rootfs_sha256.len(), 64);
        assert!(image.initrd_sha256.is_none());
        assert_eq!(image.min_disk_gb, 1);
        assert!(image.installed);

        let json = serde_json::to_string(image).unwrap();
        assert!(!json.contains("kernel/vmlinux"));
        assert!(!json.contains("rootfs/root.ext4"));
        assert!(!json.contains(root.to_str().unwrap_or("___")));
    }

    #[tokio::test]
    async fn list_images_sorts_by_alias() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        write_file(&root.join("k-z"), b"kz");
        write_file(&root.join("r-z"), b"rz");
        write_file(&root.join("k-a"), b"ka");
        write_file(&root.join("r-a"), b"ra");
        let state = state_from_specs(
            root,
            [
                TemplateSpec {
                    alias: "zebra".to_owned(),
                    version: "z-1".to_owned(),
                    kernel: Path::new("k-z").to_path_buf(),
                    initrd: None,
                    rootfs: Path::new("r-z").to_path_buf(),
                    boot_args: "a".to_owned(),
                },
                TemplateSpec {
                    alias: "alpha".to_owned(),
                    version: "a-1".to_owned(),
                    kernel: Path::new("k-a").to_path_buf(),
                    initrd: None,
                    rootfs: Path::new("r-a").to_path_buf(),
                    boot_args: "b".to_owned(),
                },
            ],
        )
        .await;

        let Json(images) = list_images(State(state)).await;
        let aliases: Vec<_> = images.iter().map(|image| image.alias.as_str()).collect();
        assert_eq!(aliases, ["alpha", "zebra"]);
    }
}
