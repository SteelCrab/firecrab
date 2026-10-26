//! Resolve only bounded OCI metadata, then pin the host-platform manifest.

use super::*;
use crate::oci::{self, ImageReference, ImageVersion};
use semver::{Version, VersionReq};

pub(super) fn reference(policy: &AutoDeployPolicy) -> Result<ImageReference, &'static str> {
    if policy.validation_error().is_some() {
        return Err("Invalid automatic Deploy policy");
    }
    let (repository, _) = policy.source().ok_or("Invalid automatic Deploy source")?;
    ImageReference::parse(&format!("{repository}:latest"))
        .map_err(|_| "Invalid automatic Deploy repository")
}

pub(super) fn select(rule: &str, tags: &[String]) -> Option<String> {
    let prefix_v = rule.starts_with('v');
    let requirement = VersionReq::parse(rule.strip_prefix('v').unwrap_or(rule)).ok()?;
    tags.iter()
        .filter_map(|tag| {
            if tag.starts_with('v') != prefix_v && rule != "*" {
                return None;
            }
            let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
            (version.pre.is_empty() && requirement.matches(&version)).then_some((version, tag))
        })
        .max_by(|(a, tag_a), (b, tag_b)| a.cmp_precedence(b).then_with(|| tag_a.cmp(tag_b)))
        .map(|(_, tag)| tag.clone())
}

pub(super) async fn discover(
    state: &AppState,
    policy: &AutoDeployPolicy,
) -> Result<(String, String), &'static str> {
    let source = reference(policy)?;
    let auth = require_image_auth(
        state,
        &format!("{}/{}:latest", source.registry, source.repository),
        Uuid::new_v4(),
    )
    .await
    .map_err(|_| "Automatic Deploy requires its connected Auth account")?;
    let credentials = auth
        .registry_credential()
        .await
        .map_err(|_| "Automatic Deploy requires available registry credentials")?;
    let (_, rule) = policy.source().ok_or("Invalid automatic Deploy source")?;
    metadata(source, rule, Some(credentials)).await
}

pub(super) async fn metadata(
    mut source: ImageReference,
    rule: &str,
    credentials: Option<oci::RegistryCredential>,
) -> Result<(String, String), &'static str> {
    let read = async {
        let tag = if rule.contains('*') {
            let mut all = Vec::new();
            let mut last = None;
            for page_number in 0..10 {
                let page = oci::registries::tags(&source, last.as_deref(), credentials.clone())
                    .await
                    .map_err(
                        |_| "Registry tag check failed; verify Auth, access and rate limits",
                    )?;
                let next = page.next_last;
                all.extend(page.tags);
                if all.len() > 1000 || (next.is_some() && next == last) {
                    return Err("Registry tag listing exceeded its safe scan limit");
                }
                if next.is_none() {
                    break;
                }
                if page_number == 9 {
                    return Err("Registry tag listing exceeded its safe scan limit");
                }
                last = next;
            }
            select(rule, &all).ok_or("No stable registry tag matches the configured rule")?
        } else {
            rule.to_owned()
        };
        source.version = ImageVersion::Tag(tag.clone());
        let resolved = oci::resolve(
            &source,
            Architecture::HOST,
            oci::is_loopback_registry(&source.registry),
            credentials,
        )
        .await
        .map_err(|_| "Registry digest check failed; verify Auth, access and rate limits")?;
        Ok((tag, resolved.digest))
    };
    tokio::time::timeout(Duration::from_secs(30), read)
        .await
        .map_err(|_| "Registry metadata check timed out")?
}
