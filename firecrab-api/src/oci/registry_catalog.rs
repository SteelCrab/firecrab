//! Provider catalogs use their metadata APIs; OCI tag queries still use the registry.
use super::{
    ImageReference, RegistryCredential, ResolveError, is_loopback_registry, read_manifest_body,
};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;

/// A page of qualified repository names from the registry's metadata API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRepositories {
    /// Repository paths without the registry hostname or an image tag.
    pub repositories: Vec<String>,
    /// The next one-based page, constructed locally instead of following remote URLs.
    pub next_page: Option<u32>,
    /// Opaque continuation token used by AWS ECR.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// False when registry credentials cannot access the provider's catalog API.
    pub listing_supported: bool,
}

/// Accept account and organization names that fit a single URL path segment.
pub fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

/// List provider repositories with credentials scoped to this exact registry.
///
/// Docker Hub uses the saved username. Harbor returns accessible project paths.
/// ECR passwords cannot list repositories, so ECR returns an unsupported page.
pub async fn repositories(
    registry: &str,
    namespace: &str,
    page: u32,
    credential: Option<RegistryCredential>,
) -> Result<RegistryRepositories, ResolveError> {
    if registry == "public.ecr.aws" || registry.contains(".dkr.ecr.") {
        // An ECR registry password cannot call the signed DescribeRepositories API.
        return Ok(RegistryRepositories {
            repositories: Vec::new(),
            next_page: None,
            next_cursor: None,
            listing_supported: false,
        });
    }
    let credential = credential.filter(|value| value.covers(registry));
    let insecure = is_loopback_registry(registry);
    let client = Client::builder()
        .https_only(!insecure)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("firecrab-pipe-registry")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(transport)?;
    if registry == "ghcr.io" {
        return github_repositories(
            &client,
            "https://api.github.com",
            namespace,
            page,
            credential.as_ref(),
        )
        .await;
    }
    if !matches!(registry, "docker.io" | "registry-1.docker.io") {
        let credential = credential.as_ref().ok_or_else(|| {
            ResolveError::Authentication("Save the Harbor registry login first".into())
        })?;
        return harbor_repositories(
            &client,
            &format!("{}://{registry}", if insecure { "http" } else { "https" }),
            page,
            credential,
        )
        .await;
    }
    let credential = credential
        .as_ref()
        .ok_or_else(|| ResolveError::Authentication("Add a Docker Hub login".into()))?;
    docker_repositories(&client, "https://hub.docker.com", page, credential).await
}

/// Remove request URLs before returning transport errors to API callers.
fn transport(error: reqwest::Error) -> ResolveError {
    ResolveError::Transport(error.without_url().to_string())
}

/// Decode a bounded successful response without exposing provider error bodies.
async fn json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ResolveError> {
    if response.status() != StatusCode::OK {
        return Err(ResolveError::Status {
            status: response.status().as_u16(),
            reference: "list registry images".into(),
        });
    }
    serde_json::from_slice(&read_manifest_body(response).await?)
        .map_err(|_| ResolveError::Malformed("invalid registry catalog response".into()))
}

/// Validate, qualify and deduplicate repository paths while retaining nested names.
fn catalog(
    namespace: &str,
    names: Vec<String>,
    more: bool,
    page: u32,
) -> Result<RegistryRepositories, ResolveError> {
    let mut repositories = Vec::with_capacity(names.len());
    for name in names {
        let repository = if namespace.is_empty() {
            name.clone()
        } else {
            format!("{}/{name}", namespace.to_ascii_lowercase())
        };
        let reference = ImageReference::parse(&format!("ghcr.io/{repository}:latest"))
            .map_err(|_| ResolveError::Malformed("invalid catalog repository name".into()))?;
        if reference.repository != repository || name.is_empty() {
            return Err(ResolveError::Malformed(
                "invalid catalog repository name".into(),
            ));
        }
        repositories.push(repository);
    }
    repositories.sort();
    repositories.dedup();
    Ok(RegistryRepositories {
        next_cursor: None,
        repositories,
        next_page: more.then_some(page + 1),
        listing_supported: true,
    })
}

/// List Harbor project repositories using the saved account's Basic authentication.
async fn harbor_repositories(
    client: &Client,
    base: &str,
    page: u32,
    credential: &RegistryCredential,
) -> Result<RegistryRepositories, ResolveError> {
    let response = client
        .get(format!("{base}/api/v2.0/repositories"))
        .query(&[("page", page), ("page_size", 100)])
        .basic_auth(&credential.username, Some(&credential.secret))
        .send()
        .await
        .map_err(transport)?;
    let more = response
        .headers()
        .get("x-total-count")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|total| total > u64::from(page) * 100)
        || response
            .headers()
            .get_all("link")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.split(',').any(|link| link.contains("rel=\"next\"")));
    #[derive(Deserialize)]
    struct Repository {
        name: String,
    }
    let repositories: Vec<Repository> = json(response).await?;
    catalog(
        "",
        repositories.into_iter().map(|value| value.name).collect(),
        more,
        page,
    )
}

/// Exchange a Docker Hub PAT and list the saved user's repositories, including private ones.
async fn docker_repositories(
    client: &Client,
    base: &str,
    page: u32,
    credential: &RegistryCredential,
) -> Result<RegistryRepositories, ResolveError> {
    let namespace = credential.username.to_ascii_lowercase();
    if !valid_namespace(&namespace) {
        return Err(ResolveError::Authentication(
            "Invalid Docker Hub username".into(),
        ));
    }
    #[derive(Deserialize)]
    struct Token {
        access_token: String,
    }
    let response = client
        .post(format!("{base}/v2/auth/token"))
        .header("content-type", "application/json")
        .body(
            serde_json::json!({"identifier": credential.username, "secret": credential.secret})
                .to_string(),
        )
        .send()
        .await
        .map_err(transport)?;
    let token: Token = json(response).await?;
    let request = client
        .get(format!("{base}/v2/namespaces/{namespace}/repositories"))
        .query(&[("page_size", 100), ("page", page)])
        .bearer_auth(token.access_token);
    #[derive(Deserialize)]
    struct Repository {
        name: String,
    }
    #[derive(Deserialize)]
    struct Page {
        results: Vec<Repository>,
        next: Option<String>,
    }
    let result: Page = json(request.send().await.map_err(transport)?).await?;
    // Construct the next page locally. Never follow a provider-supplied URL with a token.
    catalog(
        &namespace,
        result.results.into_iter().map(|value| value.name).collect(),
        result.next.is_some(),
        page,
    )
}

/// List personal or organization container packages using fixed GitHub API paths.
async fn github_repositories(
    client: &Client,
    base: &str,
    namespace: &str,
    page: u32,
    credential: Option<&RegistryCredential>,
) -> Result<RegistryRepositories, ResolveError> {
    let request = |path: &str| {
        let request = client
            .get(format!("{base}{path}"))
            .header("accept", "application/vnd.github+json");
        match credential {
            Some(credential) => request.bearer_auth(&credential.secret),
            None => request,
        }
    };
    let path = if credential.is_some_and(|value| value.username.eq_ignore_ascii_case(namespace)) {
        "/user/packages".into()
    } else {
        #[derive(Deserialize)]
        struct Owner {
            r#type: String,
        }
        let owner: Owner = json(
            request(&format!("/users/{namespace}"))
                .send()
                .await
                .map_err(transport)?,
        )
        .await?;
        let kind = if owner.r#type == "Organization" {
            "orgs"
        } else {
            "users"
        };
        format!("/{kind}/{namespace}/packages")
    };
    let response = request(&path)
        .query(&[
            ("package_type", "container".to_string()),
            ("per_page", "100".into()),
            ("page", page.to_string()),
        ])
        .send()
        .await
        .map_err(transport)?;
    let more = response
        .headers()
        .get_all("link")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.split(',').any(|link| link.contains("rel=\"next\"")));
    #[derive(Deserialize)]
    struct Package {
        name: String,
    }
    let packages: Vec<Package> = json(response).await?;
    catalog(
        namespace,
        packages.into_iter().map(|value| value.name).collect(),
        more,
        page,
    )
}

#[cfg(test)]
#[path = "registry_catalog_tests.rs"]
mod tests;
