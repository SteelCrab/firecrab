//! Compile-time M2Image release metadata shared with the shell release tools.

use std::collections::HashMap;

use serde::Deserialize;

const MANIFEST_JSON: &str = include_str!("../../packaging/m2images.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseManifest {
    pub schema_version: u8,
    pub architectures: Vec<String>,
    pub images: Vec<ReleaseImage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseImage {
    pub alias: String,
    pub distribution: String,
    pub series: String,
    pub version: String,
    pub template_version: String,
    pub builder: ReleaseBuilder,
    pub artifacts: HashMap<String, ReleaseArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseBuilder {
    pub environment: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseArtifact {
    pub kernel: String,
    pub initrd: Option<String>,
    pub rootfs: String,
    pub boot_args: String,
    pub registry_key: String,
}

pub fn load() -> ReleaseManifest {
    let manifest: ReleaseManifest =
        serde_json::from_str(MANIFEST_JSON).expect("packaging/m2images.json must be valid");
    assert_eq!(manifest.schema_version, 1, "unsupported M2Image manifest");
    assert_eq!(
        manifest.architectures,
        ["x86_64", "aarch64"],
        "M2Image manifest must cover both supported architectures"
    );
    manifest
}

pub fn image(alias: &str) -> Option<ReleaseImage> {
    load().images.into_iter().find(|image| image.alias == alias)
}

pub fn registry_key(alias: &str, architecture: &str) -> Option<String> {
    image(alias)?
        .artifacts
        .get(architecture)
        .map(|artifact| artifact.registry_key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_manifest_covers_both_host_architectures() {
        let manifest = load();
        assert_eq!(manifest.architectures, ["x86_64", "aarch64"]);
        assert_eq!(
            manifest
                .images
                .iter()
                .map(|image| image.alias.as_str())
                .collect::<Vec<_>>(),
            ["alpine-3.24.1", "ubuntu-26.04", "rocky-9.8"]
        );
        assert!(manifest.images.iter().all(|image| {
            image.artifacts.contains_key("x86_64") && image.artifacts.contains_key("aarch64")
        }));
    }
}
