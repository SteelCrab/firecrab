//! Pipe connections to external registries, with OCI image pull jobs.
use crate::{
    error::AppError,
    extract::ValidatedJson,
    oci::{self, ImageReference, RegistryCredential, registries, registry_catalog},
    server::RequestId,
    state::AppState,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use firecrab_api_types::ImageInstallResponse;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn invalid(message: impl Into<String>, id: RequestId) -> AppError {
    AppError::validation([("registry".into(), message.into())].into(), id.0)
}

fn provider(registry: &str) -> Option<&'static str> {
    if matches!(registry, "docker.io" | "registry-1.docker.io") {
        return Some("dockerHub");
    }
    if registry == "ghcr.io" {
        return Some("ghcr");
    }
    if registry == "public.ecr.aws" {
        return Some("ecr");
    }
    let host = registry
        .strip_suffix(".amazonaws.com")
        .or_else(|| registry.strip_suffix(".amazonaws.com.cn"))?;
    let parts: Vec<_> = host.split('.').collect();
    if parts.len() != 4
        || parts[0].len() != 12
        || !parts[0].bytes().all(|c| c.is_ascii_digit())
        || parts[1..3] != ["dkr", "ecr"]
        || parts[3].is_empty()
        || !parts[3]
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        return None;
    }
    Some("ecr")
}

fn reference(value: &str, id: RequestId) -> Result<ImageReference, AppError> {
    let reference = ImageReference::parse(value).map_err(|error| invalid(error.to_string(), id))?;
    if !reference.version.is_immutable() && !registries::valid_tag(reference.version.as_str()) {
        return Err(invalid("Invalid image tag", id));
    }
    Ok(reference)
}

/// Accept a hostname and optional port, excluding URL credentials, paths and queries.
fn valid_harbor_host(registry: &str) -> bool {
    if registry.is_empty()
        || registry.len() > 255
        || !registry.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
    {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(&format!("https://{registry}/")) else {
        return false;
    };
    url.host_str().is_some()
        && url.port() != Some(0)
        && (registry.contains('.') || registry.contains(':') || registry == "localhost")
}

/// Resolve an image and its login, requiring registration for custom Harbor hosts.
async fn registered_reference(
    state: &AppState,
    value: &str,
    id: RequestId,
) -> Result<(ImageReference, Option<RegistryCredential>), AppError> {
    let reference = reference(value, id)?;
    let credential = super::oci::stored_credential_for(state, &reference.registry, id).await;
    if provider(&reference.registry).is_none() && credential.is_none() {
        return Err(invalid(
            "Add this Harbor registry before querying or pulling its images",
            id,
        ));
    }
    Ok((reference, credential))
}

#[derive(Serialize)]
pub struct Connection {
    registry: String,
    username: String,
    provider: &'static str,
}
fn connection(credential: RegistryCredential) -> Connection {
    Connection {
        provider: provider(&credential.registry).unwrap_or("harbor"),
        registry: if credential.registry == "registry-1.docker.io" {
            "docker.io".into()
        } else {
            credential.registry
        },
        username: credential.username,
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<Connection>>, AppError> {
    let rows = tokio::task::spawn_blocking(move || state.store.registry_credentials())
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(rows.into_iter().map(connection).collect()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    username: String,
    secret: String,
    provider: Option<String>,
}
pub async fn save(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(registry): Path<String>,
    ValidatedJson(body): ValidatedJson<Login>,
) -> Result<Json<Connection>, AppError> {
    let registry = registry.trim().to_ascii_lowercase();
    let kind = match provider(&registry) {
        Some(kind) => kind,
        None if body.provider.as_deref() == Some("harbor") && valid_harbor_host(&registry) => {
            "harbor"
        }
        None => {
            return Err(invalid(
                "Choose Harbor and provide its registry hostname",
                id,
            ));
        }
    };
    let username = if kind == "ecr" {
        "AWS"
    } else {
        body.username.trim()
    };
    let secret = body.secret.trim();
    if username.is_empty()
        || username.len() > 255
        || username.contains(':')
        || username.chars().any(char::is_control)
        || secret.is_empty()
        || secret.len() > 16384
        || secret.chars().any(char::is_control)
    {
        return Err(invalid("Provide a username and a valid registry token", id));
    }
    let credential = RegistryCredential {
        registry,
        username: username.into(),
        secret: secret.into(),
    };
    let response = connection(credential.clone());
    tokio::task::spawn_blocking(move || state.store.put_registry_credential(&credential))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(response))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(registry): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let registry = registry.trim().to_ascii_lowercase();
    if provider(&registry).is_none() && !valid_harbor_host(&registry) {
        return Err(invalid("Unknown registry", id));
    }
    tokio::task::spawn_blocking(move || state.store.delete_registry_credential(&registry))
        .await
        .map_err(|_| AppError::internal(id.0))?
        .map_err(|_| AppError::internal(id.0))?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

/// Pagination and the optional GitHub account or organization override.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoriesQuery {
    /// GitHub catalog owner; Docker Hub always uses the saved account instead.
    namespace: Option<String>,
    /// One-based catalog page, defaulting to the first page.
    page: Option<u32>,
}

/// Return a provider catalog page after validating the host, account and page number.
pub async fn repositories(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(registry): Path<String>,
    Query(query): Query<RepositoriesQuery>,
) -> Result<Json<registry_catalog::RegistryRepositories>, AppError> {
    let registry = registry.trim().to_ascii_lowercase();
    let page = query.page.unwrap_or(1);
    if !(1..=10000).contains(&page) {
        return Err(invalid("Invalid repository page", id));
    }
    let credential = super::oci::stored_credential_for(&state, &registry, id).await;
    let kind = match provider(&registry) {
        Some(kind) => kind,
        None if valid_harbor_host(&registry) && credential.is_some() => "harbor",
        None => return Err(invalid("Unknown registry", id)),
    };
    let namespace = if kind == "dockerHub" {
        credential
            .as_ref()
            .ok_or_else(|| invalid("Add a Docker Hub login", id))?
            .username
            .as_str()
    } else {
        query
            .namespace
            .as_deref()
            .or_else(|| credential.as_ref().map(|value| value.username.as_str()))
            .unwrap_or("")
    }
    .trim()
    .to_owned();
    if !matches!(kind, "ecr" | "harbor") && !registry_catalog::valid_namespace(&namespace) {
        return Err(invalid("Provide an account or organization name", id));
    }
    registry_catalog::repositories(&registry, &namespace, page, credential)
        .await
        .map(Json)
        .map_err(|error| invalid(error.to_string(), id))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TagsQuery {
    reference: String,
    last: Option<String>,
}
pub async fn tags(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Query(query): Query<TagsQuery>,
) -> Result<Json<registries::RegistryTags>, AppError> {
    let (reference, credential) = registered_reference(&state, &query.reference, id).await?;
    if query
        .last
        .as_deref()
        .is_some_and(|tag| !registries::valid_tag(tag))
    {
        return Err(invalid("Invalid tag cursor", id));
    }
    registries::tags(&reference, query.last.as_deref(), credential)
        .await
        .map(Json)
        .map_err(|error| invalid(error.to_string(), id))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryImage {
    #[serde(flatten)]
    image: registries::LocalRegistryImage,
    template_alias: String,
}

fn registry_image(
    image: registries::LocalRegistryImage,
    id: RequestId,
) -> Result<RegistryImage, AppError> {
    let mut pinned = reference(&image.reference, id)?;
    pinned.version = oci::ImageVersion::Digest(
        oci::Sha256Digest::parse(&image.digest).map_err(|error| invalid(error.to_string(), id))?,
    );
    let named = oci::template_name_from_reference(&pinned)
        .map_err(|error| invalid(error.to_string(), id))?;
    Ok(RegistryImage {
        image,
        template_alias: named.alias,
    })
}

pub async fn images(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
) -> Result<Json<Vec<RegistryImage>>, AppError> {
    registries::images(state.templates.image_root_path())
        .await
        .map_err(|error| invalid(error.to_string(), id))?
        .into_iter()
        .map(|image| registry_image(image, id))
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

/// The `action` tag keeps the request explicit; pulling is the only transfer.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum Transfer {
    Pull { reference: String },
}
pub async fn transfer(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    ValidatedJson(Transfer::Pull { reference: value }): ValidatedJson<Transfer>,
) -> Result<(StatusCode, Json<ImageInstallResponse>), AppError> {
    let (reference, credential) = registered_reference(&state, &value, id).await?;
    let permit = state
        .registry_transfer_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::conflict(
                "transfer_busy",
                "Two registry transfers are already running",
                id.0,
            )
        })?;
    let key = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "pull:{}:{}:{}",
                reference.registry,
                reference.repository,
                reference.version.as_str()
            )
            .as_bytes()
        )
    );
    let response = state
        .registry_transfers
        .begin_with(&key, "registry transfer started")
        .map_err(|_| {
            AppError::conflict(
                "transfer_in_progress",
                "This transfer is already running",
                id.0,
            )
        })?;
    let root = state.templates.image_root_path().to_path_buf();
    tokio::spawn(async move {
        let _permit = permit;
        match registries::pull(&root, &reference, credential).await {
            Ok(image) => state.registry_transfers.finish_ok_with(
                &key,
                format!("registry transfer succeeded: {}", image.digest),
            ),
            Err(error) => state
                .registry_transfers
                .finish_err_with(&key, format!("registry transfer failed: {error}")),
        }
    });
    Ok((StatusCode::ACCEPTED, Json(response)))
}

pub async fn job(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    Path(key): Path<String>,
) -> Result<Json<ImageInstallResponse>, AppError> {
    oci::Sha256Digest::parse(&format!("sha256:{key}"))
        .map_err(|_| invalid("Invalid transfer ID", id))?;
    Ok(Json(state.registry_transfers.snapshot(&key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_image_alias_is_pinned_to_the_downloaded_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for (reference, expected) in [
            ("docker.io/team/app:latest", "team-app-sha256-aaaaaaaaaaaa"),
            (
                "127.0.0.1:18080/project/app:latest",
                "127.0.0.1-18080-project-app-sha256-aaaaaaaaaaaa",
            ),
        ] {
            let image = registry_image(
                registries::LocalRegistryImage {
                    id: "a".repeat(64),
                    reference: reference.into(),
                    digest: digest.clone(),
                    architecture: "aarch64".into(),
                    size_bytes: 1024,
                },
                RequestId(uuid::Uuid::nil()),
            )
            .unwrap();
            assert_eq!(image.template_alias, expected);
            let json = serde_json::to_value(image).unwrap();
            assert_eq!(json["templateAlias"], expected);
            assert_eq!(json["reference"], reference);
            assert_eq!(json["digest"], digest);
        }
    }

    #[test]
    fn docker_hub_references_normalize_the_registry_and_official_image_namespace() {
        let id = RequestId(uuid::Uuid::nil());
        for value in [
            "nginx:alpine",
            "docker.io/nginx:alpine",
            "docker.io/library/nginx:alpine",
            "registry-1.docker.io/library/nginx:alpine",
        ] {
            let parsed = reference(value, id).unwrap();
            assert_eq!(parsed.registry, "registry-1.docker.io");
            assert_eq!(parsed.repository, "library/nginx");
            assert_eq!(parsed.version.as_str(), "alpine");
        }
        let parsed = reference("docker.io/team/private:v1", id).unwrap();
        assert_eq!(parsed.repository, "team/private");
        assert!(reference("docker.io/nginx:tag?query", id).is_err());
    }

    #[test]
    fn limits_logins_and_references_to_provider_hosts_and_valid_tags() {
        for host in [
            "docker.io",
            "registry-1.docker.io",
            "ghcr.io",
            "public.ecr.aws",
            "123456789012.dkr.ecr.us-east-1.amazonaws.com",
            "123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn",
        ] {
            assert!(provider(host).is_some(), "{host}");
        }
        for host in [
            "docker.io.evil.com",
            "registry-1.docker.io.evil.com",
            "docker.io:5000",
            "registry-1.docker.io:5000",
            "ghcr.io.evil.com",
            "https://ghcr.io",
            "ghcr.io:443",
            "123.dkr.ecr.us-east-1.amazonaws.com",
            "123456789012.dkr.ecr..amazonaws.com",
        ] {
            assert!(provider(host).is_none(), "{host}");
        }
        for value in [
            "ghcr.io/team/app:tag?query",
            "ghcr.io/team/app:tag#fragment",
            "https://ghcr.io/team/app",
        ] {
            assert!(reference(value, RequestId(uuid::Uuid::nil())).is_err());
        }
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use crate::templates::TemplateRegistry;

    #[tokio::test]
    async fn harbor_registry_requires_registration_and_keeps_its_login_write_only() {
        let root = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
        let state = AppState::with_db_file(templates, root.path().join("state.db"))
            .await
            .unwrap();
        let id = RequestId(uuid::Uuid::nil());
        assert!(
            registered_reference(&state, "harbor.example.com/firecrab/nginx:latest", id)
                .await
                .is_err()
        );
        let Json(saved) = save(
            State(state.clone()),
            Extension(id),
            Path("harbor.example.com".into()),
            ValidatedJson(Login {
                provider: Some("harbor".into()),
                username: "reader".into(),
                secret: "harbor-secret".into(),
            }),
        )
        .await
        .unwrap();
        let json = serde_json::to_value(saved).unwrap();
        assert_eq!(json["provider"], "harbor");
        assert!(!json.to_string().contains("harbor-secret"));
        let (_, login) =
            registered_reference(&state, "harbor.example.com/firecrab/nginx:latest", id)
                .await
                .unwrap();
        assert_eq!(login.unwrap().username, "reader");
        let Json(rows) = list(State(state.clone()), Extension(id)).await.unwrap();
        assert_eq!(rows[0].provider, "harbor");
        let Json(_) = delete(
            State(state.clone()),
            Extension(id),
            Path("harbor.example.com".into()),
        )
        .await
        .unwrap();
        assert!(
            registered_reference(&state, "harbor.example.com/firecrab/nginx:latest", id)
                .await
                .is_err()
        );
        for registry in [
            "https://harbor.example.com",
            "harbor.example.com/path",
            "user@harbor.example.com",
            "harbor.example.com:0",
            "harbor.example.com\\evil",
            "harbor.example.com\n",
        ] {
            assert!(!valid_harbor_host(registry), "{registry}");
        }
    }

    #[tokio::test]
    async fn saves_write_only_provider_logins_and_uses_them_for_oci_import() {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
        let state = AppState::with_db_file(templates, dir.path().join("state.db"))
            .await
            .unwrap();
        let id = RequestId(uuid::Uuid::nil());
        let ecr = "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com";
        for (registry, username, secret) in [
            (" GHCR.IO ", " octocat ", " ghp_secret "),
            (ecr, "ignored", "ecr_secret"),
        ] {
            let Json(saved) = save(
                State(state.clone()),
                Extension(id),
                Path(registry.into()),
                ValidatedJson(Login {
                    provider: None,
                    username: username.into(),
                    secret: secret.into(),
                }),
            )
            .await
            .unwrap();
            let json = serde_json::to_value(saved).unwrap();
            assert!(json.get("secret").is_none());
            assert!(!json.to_string().contains(secret.trim()));
        }
        let Json(rows) = list(State(state.clone()), Extension(id)).await.unwrap();
        assert_eq!(rows.len(), 2);
        let login = super::super::oci::stored_credential_for(&state, ecr, id)
            .await
            .unwrap();
        assert_eq!(login.username, "AWS");
        assert_eq!(login.secret, "ecr_secret");
        assert!(!format!("{login:?}").contains("ecr_secret"));
        let login = super::super::oci::stored_credential_for(&state, "ghcr.io", id)
            .await
            .unwrap();
        assert_eq!(login.username, "octocat");
        assert_eq!(login.secret, "ghp_secret");
        assert!(
            super::super::oci::stored_credential_for(&state, "untrusted.invalid", id)
                .await
                .is_none()
        );
        let _ = delete(State(state.clone()), Extension(id), Path("ghcr.io".into()))
            .await
            .unwrap();
        assert!(
            super::super::oci::stored_credential_for(&state, "ghcr.io", id)
                .await
                .is_none()
        );
        assert!(
            super::super::oci::stored_credential_for(&state, ecr, id)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn rejects_empty_logins_and_push_requests_before_starting_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
        let state = AppState::with_db_file(templates, dir.path().join("state.db"))
            .await
            .unwrap();
        let id = RequestId(uuid::Uuid::nil());
        assert!(
            save(
                State(state.clone()),
                Extension(id),
                Path("ghcr.io".into()),
                ValidatedJson(Login {
                    provider: None,
                    username: "octocat".into(),
                    secret: "  \n ".into()
                })
            )
            .await
            .is_err()
        );
        assert!(serde_json::from_value::<Transfer>(serde_json::json!({"action":"push","imageId":"a".repeat(64),"reference":"ghcr.io/team/app:v1"})).is_err());
        assert!(serde_json::from_value::<Transfer>(serde_json::json!({"action":"pull","reference":"ghcr.io/team/app:v1","command":"ignored"})).is_err());
        assert_eq!(state.registry_transfer_permits.available_permits(), 2);
    }
    #[tokio::test]
    async fn docker_hub_login_is_shared_with_oci_and_listed_without_duplicate_aliases_or_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(dir.path(), std::iter::empty()).unwrap();
        let state = AppState::with_db_file(templates, dir.path().join("state.db"))
            .await
            .unwrap();
        let id = RequestId(uuid::Uuid::nil());
        state
            .store
            .put_docker_hub_credential("legacy-user", "legacy-token")
            .unwrap();
        let Json(rows) = list(State(state.clone()), Extension(id)).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].registry, "docker.io");
        assert_eq!(rows[0].provider, "dockerHub");
        assert!(
            !serde_json::to_string(&rows)
                .unwrap()
                .contains("legacy-token")
        );
        for host in [" docker.io ", "REGISTRY-1.DOCKER.IO"] {
            let Json(saved) = save(
                State(state.clone()),
                Extension(id),
                Path(host.into()),
                ValidatedJson(Login {
                    provider: None,
                    username: " octocat ".into(),
                    secret: " dckr_pat_secret ".into(),
                }),
            )
            .await
            .unwrap();
            assert_eq!(saved.registry, "docker.io");
            assert_eq!(saved.username, "octocat");
            assert_eq!(saved.provider, "dockerHub");
            let Json(rows) = list(State(state.clone()), Extension(id)).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert!(
                !serde_json::to_string(&rows)
                    .unwrap()
                    .contains("dckr_pat_secret")
            );
            for host in ["docker.io", "registry-1.docker.io"] {
                let login = super::super::oci::stored_credential_for(&state, host, id)
                    .await
                    .unwrap();
                assert_eq!(login.secret, "dckr_pat_secret");
            }
        }
        let _ = delete(
            State(state.clone()),
            Extension(id),
            Path("docker.io".into()),
        )
        .await
        .unwrap();
        assert!(state.store.docker_hub_credential().unwrap().is_none());
        assert!(
            super::super::oci::stored_credential_for(&state, "registry-1.docker.io", id)
                .await
                .is_none()
        );
    }
}
