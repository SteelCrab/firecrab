//! OCI images pulled from external registries and retained on this host.
use super::*;
use reqwest::StatusCode;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalRegistryImage {
    pub id: String,
    pub reference: String,
    pub digest: String,
    pub architecture: String,
    pub size_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryTags {
    pub tags: Vec<String>,
    pub next_last: Option<String>,
}

pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        && tag
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn validate_platform(config: &[u8], expected: Architecture) -> Result<(), ResolveError> {
    #[derive(Deserialize)]
    struct Platform {
        os: String,
        architecture: String,
    }
    let platform: Platform = serde_json::from_slice(config).map_err(|_| {
        ResolveError::MalformedConfig("image configuration must declare os and architecture".into())
    })?;
    let architecture = match expected {
        Architecture::X86_64 => "amd64",
        Architecture::Aarch64 => "arm64",
    };
    if platform.os != LINUX || platform.architecture != architecture {
        return Err(ResolveError::MalformedConfig(format!(
            "image platform {}/{} does not match this host (linux/{architecture})",
            platform.os, platform.architecture
        )));
    }
    Ok(())
}

pub async fn tags(
    reference: &ImageReference,
    last: Option<&str>,
    credential: Option<RegistryCredential>,
) -> Result<RegistryTags, ResolveError> {
    let session = RegistrySession::new(
        reference,
        is_loopback_registry(&reference.registry),
        credential,
    )?;
    let mut url = reqwest::Url::parse(&format!(
        "{}/v2/{}/tags/list",
        session.base, reference.repository
    ))
    .map_err(|_| ResolveError::Malformed("invalid repository URL".into()))?;
    url.query_pairs_mut().append_pair("n", "100");
    if let Some(last) = last {
        url.query_pairs_mut().append_pair("last", last);
    }
    let response = session
        .get(url.as_str(), None, Some(Duration::from_secs(20)))
        .await?;
    expect_status(&response, StatusCode::OK, "list tags")?;
    let more = response.headers().contains_key("link");
    #[derive(Deserialize)]
    struct Tags {
        tags: Option<Vec<String>>,
    }
    let tags: Tags = serde_json::from_slice(&read_manifest_body(response).await?)
        .map_err(|_| ResolveError::Malformed("invalid registry tags response".into()))?;
    let tags = tags.tags.unwrap_or_default();
    if tags.iter().any(|tag| !valid_tag(tag)) {
        return Err(ResolveError::Malformed(
            "registry returned an invalid tag".into(),
        ));
    }
    let next_last = more.then(|| tags.last().cloned()).flatten();
    Ok(RegistryTags { tags, next_last })
}

fn image_dir(root: &Path) -> PathBuf {
    root.join(".oci/registry-images")
}

pub async fn images(root: &Path) -> Result<Vec<LocalRegistryImage>, ResolveError> {
    let dir = image_dir(root);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(cache_io("list registry images", dir, error)),
    };
    let mut images = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| cache_io("list", dir.clone(), error))?
    {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|error| cache_io("read image", path, error))?;
            images.push(
                serde_json::from_slice::<LocalRegistryImage>(&bytes)
                    .map_err(|_| ResolveError::Malformed("invalid saved OCI image".into()))?,
            );
        }
    }
    images.sort_by(|a, b| a.reference.cmp(&b.reference));
    Ok(images)
}

pub async fn pull(
    root: &Path,
    reference: &ImageReference,
    credential: Option<RegistryCredential>,
) -> Result<LocalRegistryImage, ResolveError> {
    let insecure = is_loopback_registry(&reference.registry);
    let session = RegistrySession::new(reference, insecure, credential.clone())?;
    // Resolve once, then pin the blob download to that digest.
    let resolved = resolve_manifest(&session, reference, Architecture::HOST).await?;
    let digest = Sha256Digest::parse(&resolved.resolved.digest)?;
    let pinned = ImageReference {
        version: ImageVersion::Digest(digest.clone()),
        ..reference.clone()
    };
    let cached = cache_image_blobs(
        &pinned,
        Architecture::HOST,
        insecure,
        &BlobCache::new(root),
        credential,
    )
    .await?;
    if cached.config.descriptor.size > CONFIG_MAX_BYTES {
        return Err(ResolveError::MalformedConfig(
            "image configuration exceeds parse limit".into(),
        ));
    }
    let mut config = Vec::new();
    tokio::fs::File::open(&cached.config.path)
        .await
        .map_err(|error| cache_io("open config", cached.config.path.clone(), error))?
        .take(CONFIG_MAX_BYTES + 1)
        .read_to_end(&mut config)
        .await
        .map_err(|error| cache_io("read config", cached.config.path.clone(), error))?;
    if config.len() as u64 > CONFIG_MAX_BYTES {
        return Err(ResolveError::MalformedConfig(
            "image configuration exceeds parse limit".into(),
        ));
    }
    validate_platform(&config, Architecture::HOST)?;
    let image = LocalRegistryImage {
        id: digest.encoded().into(),
        reference: format!(
            "{}/{}{}{}",
            reference.registry,
            reference.repository,
            if reference.version.is_immutable() {
                "@"
            } else {
                ":"
            },
            reference.version.as_str()
        ),
        digest: digest.to_string(),
        architecture: cached.resolved.architecture.as_str().into(),
        size_bytes: std::iter::once(&cached.config)
            .chain(&cached.layers)
            .map(|blob| blob.descriptor.size)
            .sum(),
    };
    let dir = image_dir(root);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|error| cache_io("create image directory", dir.clone(), error))?;
    let temp = dir.join(format!("{}.tmp", Uuid::new_v4()));
    tokio::fs::write(
        &temp,
        serde_json::to_vec(&image).expect("serializable image"),
    )
    .await
    .map_err(|error| cache_io("write image", temp.clone(), error))?;
    tokio::fs::rename(&temp, dir.join(format!("{}.json", image.id)))
        .await
        .map_err(|error| cache_io("publish image", temp, error))?;
    Ok(image)
}

fn expect_status(
    response: &reqwest::Response,
    expected: StatusCode,
    operation: &str,
) -> Result<(), ResolveError> {
    if response.status() != expected {
        return Err(ResolveError::Status {
            status: response.status().as_u16(),
            reference: operation.into(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "registry_transfer_tests.rs"]
mod tests;
