//! Derives a unique template alias and version from an OCI reference.
//!
//! `TemplateSpec` needs both. A reference already has a repository and a tag
//! or digest, so this stage turns those into the same shape built-in images
//! use and refuses a name that would overwrite an installed or catalog image.
//! It does not register anything.

use super::*;

use crate::templates::TemplateRegistry;

/// Hex characters of a digest kept in a derived alias.
const DIGEST_ALIAS_HEX: usize = 12;

/// Builds the candidate alias and version without consulting the registry.
pub(super) fn template_name_from_reference(
    reference: &ImageReference,
) -> Result<OciTemplateName, ResolveError> {
    let alias = alias_from_reference(reference)?;
    let version = match &reference.version {
        ImageVersion::Tag(tag) => tag.clone(),
        ImageVersion::Digest(digest) => digest.as_str().to_owned(),
    };
    Ok(OciTemplateName { alias, version })
}

/// Derives the name and refuses it when an installed or reserved alias exists.
pub(super) fn claim_template_name(
    reference: &ImageReference,
    templates: &TemplateRegistry,
) -> Result<OciTemplateName, ResolveError> {
    let named = template_name_from_reference(reference)?;
    if let Some(occupant) = occupied_by(&named.alias, templates) {
        return Err(ResolveError::AliasCollision {
            alias: named.alias,
            occupant,
        });
    }
    Ok(named)
}

/// Attaches a claimed name to a paired image. The pair is left unchanged.
pub(super) fn name_oci_image(
    image: OciBootableImage,
    reference: &ImageReference,
    templates: &TemplateRegistry,
) -> Result<NamedOciImage, ResolveError> {
    let named = claim_template_name(reference, templates)?;
    Ok(NamedOciImage {
        image,
        alias: named.alias,
        version: named.version,
    })
}

fn occupied_by(alias: &str, templates: &TemplateRegistry) -> Option<String> {
    if templates.resolve_alias(alias).is_some() {
        return Some(alias.to_owned());
    }
    TemplateRegistry::known_spec(alias).map(|spec| spec.alias)
}

fn alias_from_reference(reference: &ImageReference) -> Result<String, ResolveError> {
    let mut parts = Vec::new();
    if reference.registry != DOCKER_HUB_REGISTRY {
        parts.push(sanitize_segment(&reference.registry));
    }
    let repository = docker_hub_repository(&reference.registry, &reference.repository);
    parts.push(sanitize_segment(&repository.replace('/', "-")));
    parts.push(version_segment(&reference.version));

    let alias = parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let alias = collapse_dashes(&alias);
    if alias.is_empty() || !is_valid_alias(&alias) {
        return Err(ResolveError::AliasUnusable {
            reference: display_reference(reference),
        });
    }
    Ok(alias)
}

fn docker_hub_repository<'a>(registry: &str, repository: &'a str) -> &'a str {
    if registry == DOCKER_HUB_REGISTRY {
        repository
            .strip_prefix(&format!("{DOCKER_HUB_LIBRARY}/"))
            .unwrap_or(repository)
    } else {
        repository
    }
}

fn version_segment(version: &ImageVersion) -> String {
    match version {
        ImageVersion::Tag(tag) => sanitize_segment(tag),
        ImageVersion::Digest(digest) => {
            let hex = digest.encoded();
            let short = &hex[..DIGEST_ALIAS_HEX.min(hex.len())];
            format!("sha256-{short}")
        }
    }
}

fn sanitize_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        let mapped = if byte.is_ascii_uppercase() {
            (byte - b'A' + b'a') as char
        } else if byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_')
        {
            byte as char
        } else {
            '-'
        };
        out.push(mapped);
    }
    collapse_dashes(&out)
}

fn collapse_dashes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut previous_dash = false;
    for ch in value.chars() {
        if ch == '-' {
            if !previous_dash && !out.is_empty() {
                out.push('-');
            }
            previous_dash = true;
        } else {
            out.push(ch);
            previous_dash = false;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

fn is_valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias
            .split('-')
            .all(|part| !part.is_empty() && is_valid_component(part))
}

fn display_reference(reference: &ImageReference) -> String {
    match &reference.version {
        ImageVersion::Tag(tag) => format!(
            "{}/{repository}:{tag}",
            reference.registry,
            repository = reference.repository
        ),
        ImageVersion::Digest(digest) => format!(
            "{}/{repository}@{digest}",
            reference.registry,
            repository = reference.repository
        ),
    }
}
