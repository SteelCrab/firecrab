//! Provider credentials are shared by Pipe services. Only connection metadata is public.
use crate::oci::RegistryCredential;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use reqwest::{
    Client, Response,
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

/// External account providers whose credentials can be reused by Pipe services.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Provider {
    /// AWS signing credentials for a private ECR registry.
    Aws,
    /// GitHub classic personal access token for GHCR.
    Github,
}
impl Provider {
    /// Stable provider key used by URL paths and credential database rows.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aws => "aws",
            Self::Github => "github",
        }
    }
}

/// Public connection metadata; no credential values are serialized into this view.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    /// Provider that verified the account credentials.
    pub provider: Provider,
    /// Verified GitHub login or twelve-digit AWS account ID.
    pub account: String,
    /// Exact registry hostname covered by this connection.
    pub registry: String,
    /// Selected AWS region, or `None` for GitHub.
    pub region: Option<String>,
    /// Pipe service identifiers that can reuse this connection.
    pub services: Vec<&'static str>,
}

/// AWS signing material and the region used for ECR requests.
///
/// Contains secrets, so it deliberately omits `Debug` and must stay out of API responses.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AwsCredentials {
    /// IAM access key ID identifying the principal that signs each request.
    pub access_key_id: String,
    /// Secret key used to derive the SigV4 signing key.
    pub secret_access_key: String,
    /// Temporary session token, or an empty string for long-lived access keys.
    #[serde(default)]
    pub session_token: String,
    /// Region used for the signing scope and ECR endpoint hostname.
    pub region: String,
}

/// Provider-specific credential document submitted when connecting or rotating an account.
#[derive(Deserialize)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Login {
    /// Verify a classic PAT and its package access against the GitHub account API.
    Github {
        /// Secret token submitted to GitHub to establish the account identity.
        token: String,
    },
    /// Verify AWS signing credentials by requesting an ECR authorization token.
    Aws {
        /// IAM access key ID identifying the signing principal.
        access_key_id: String,
        /// Secret key used for the SigV4 request signature.
        secret_access_key: String,
        /// Optional token accompanying temporary AWS credentials.
        #[serde(default)]
        session_token: String,
        /// AWS region whose ECR API verifies the credentials.
        region: String,
    },
}
impl Login {
    /// Provider selected by the credential document, checked against the URL path.
    pub fn provider(&self) -> Provider {
        match self {
            Self::Github { .. } => Provider::Github,
            Self::Aws { .. } => Provider::Aws,
        }
    }
}

/// Verified provider credentials persisted in the private credential database.
///
/// Public responses must use [`Self::connection`] instead of serializing this value.
#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "provider",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SavedAuth {
    /// GitHub identity and the classic PAT that authenticated it.
    Github {
        /// Verified login used for registry authentication and catalog ownership.
        username: String,
        /// Secret PAT retained for subsequent GHCR requests.
        token: String,
    },
    /// AWS account identity and credentials used to obtain fresh ECR logins.
    Aws {
        /// Account ID derived from the verified ECR proxy endpoint.
        account_id: String,
        /// Signing material and region retained for ECR metadata and token requests.
        credentials: AwsCredentials,
    },
}
impl SavedAuth {
    /// Project a stored credential into the secret-free dashboard connection view.
    pub fn connection(&self) -> Connection {
        match self {
            Self::Github { username, .. } => Connection {
                provider: Provider::Github,
                account: username.clone(),
                registry: "ghcr.io".into(),
                region: None,
                services: vec!["registry"],
            },
            Self::Aws {
                account_id,
                credentials,
            } => Connection {
                provider: Provider::Aws,
                account: account_id.clone(),
                registry: ecr_registry(account_id, &credentials.region),
                region: Some(credentials.region.clone()),
                services: vec!["registry", "storage"],
            },
        }
    }

    /// Return a login scoped to this account's exact registry hostname.
    ///
    /// AWS exchanges the saved keys for a fresh ECR password and rejects an account change.
    pub async fn registry_credential(&self) -> Result<RegistryCredential, &'static str> {
        match self {
            Self::Github { username, token } => Ok(RegistryCredential {
                registry: "ghcr.io".into(),
                username: username.clone(),
                secret: token.clone(),
            }),
            Self::Aws {
                account_id,
                credentials,
            } => {
                validate_aws(credentials)?;
                let host = ecr_api_host(&credentials.region);
                let credential =
                    ecr_credential(&client()?, credentials, &format!("https://{host}/")).await?;
                if credential.registry != ecr_registry(account_id, &credentials.region) {
                    return Err("AWS account changed. Reconnect AWS in Auth.");
                }
                Ok(credential)
            }
        }
    }
}

/// Build a bounded HTTPS client that never follows credential-bearing redirects.
fn client() -> Result<Client, &'static str> {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(6))
        .user_agent("firecrab-pipe-auth")
        .build()
        .map_err(|_| "Could not create authentication client")
}

/// Verify submitted credentials and return the identity and secrets suitable for storage.
pub async fn connect(login: Login) -> Result<SavedAuth, &'static str> {
    match login {
        Login::Github { token } => {
            validate_secret(&token, 16384)?;
            github_auth(&client()?, &token, "https://api.github.com/user").await
        }
        Login::Aws {
            access_key_id,
            secret_access_key,
            session_token,
            region,
        } => {
            let credentials = AwsCredentials {
                access_key_id,
                secret_access_key,
                session_token,
                region,
            };
            validate_aws(&credentials)?;
            let host = ecr_api_host(&credentials.region);
            let credential =
                ecr_credential(&client()?, &credentials, &format!("https://{host}/")).await?;
            let account_id = credential
                .registry
                .split('.')
                .next()
                .unwrap_or_default()
                .to_owned();
            Ok(SavedAuth::Aws {
                account_id,
                credentials,
            })
        }
    }
}

/// Reject empty, oversized, or non-graphic ASCII secrets before constructing HTTP headers.
fn validate_secret(value: &str, max: usize) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > max || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("Provide a valid credential without spaces or control characters");
    }
    Ok(())
}

/// Validate signing keys, the optional session token, and the region hostname component.
pub(crate) fn validate_aws(credentials: &AwsCredentials) -> Result<(), &'static str> {
    if !(16..=128).contains(&credentials.access_key_id.len())
        || !credentials
            .access_key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("Provide a valid AWS access key ID");
    }
    validate_secret(&credentials.secret_access_key, 128)?;
    if !credentials.session_token.is_empty() {
        validate_secret(&credentials.session_token, 16384)?;
    }
    validate_region(&credentials.region)
}

/// Validate the AWS region used in fixed provider hostnames and SigV4 scopes.
pub(crate) fn validate_region(region: &str) -> Result<(), &'static str> {
    let parts: Vec<_> = region.split('-').collect();
    if region.len() > 63
        || parts.len() < 3
        || parts.len() > 5
        || parts.iter().any(|part| part.is_empty())
        || !parts
            .last()
            .unwrap_or(&"")
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("Provide an AWS region such as ap-northeast-2");
    }
    Ok(())
}

/// Select the AWS DNS suffix, including the separate China region suffix.
fn aws_domain(region: &str) -> &'static str {
    if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    }
}
/// Construct the regional ECR control-plane hostname used by signed metadata requests.
fn ecr_api_host(region: &str) -> String {
    format!("api.ecr.{region}.{}", aws_domain(region))
}
/// Construct the private registry hostname for one verified AWS account and region.
fn ecr_registry(account: &str, region: &str) -> String {
    format!("{account}.dkr.ecr.{region}.{}", aws_domain(region))
}

/// Verify the GitHub account identity and classic PAT package scope at the given endpoint.
async fn github_auth(
    client: &Client,
    token: &str,
    endpoint: &str,
) -> Result<SavedAuth, &'static str> {
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Could not reach GitHub. Retry the connection.")?;
    if !response.status().is_success() {
        return Err("GitHub rejected the token. Check its validity and permissions.");
    }
    let scopes = response
        .headers()
        .get("x-oauth-scopes")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !scopes
        .split(',')
        .any(|scope| matches!(scope.trim(), "read:packages" | "write:packages"))
    {
        return Err("Use a GitHub personal access token (classic) with read:packages permission.");
    }
    /// Account identity returned by the GitHub user endpoint.
    #[derive(Deserialize)]
    struct User {
        /// Login name used for GHCR authentication and the default package namespace.
        login: String,
    }
    let user: User = response_json(response).await?;
    if !crate::oci::registry_catalog::valid_namespace(&user.login) {
        return Err("GitHub returned an invalid account");
    }
    Ok(SavedAuth::Github {
        username: user.login,
        token: token.into(),
    })
}

/// Decode a provider response with a 128 KiB limit and errors that omit response contents.
async fn response_json<T: DeserializeOwned>(mut response: Response) -> Result<T, &'static str> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read authentication response")?
    {
        if body.len() + chunk.len() > 131072 {
            return Err("Authentication response is too large");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| "Provider returned an invalid authentication response")
}

/// Fixed JSON POST signing for ECR. Header names are sorted before hashing (AWS SigV4).
fn ecr_headers(
    credentials: &AwsCredentials,
    timestamp: &str,
    operation: &str,
    body: &str,
) -> HeaderMap {
    let host = ecr_api_host(&credentials.region);
    let mut values = BTreeMap::from([
        ("content-type", "application/x-amz-json-1.1".to_owned()),
        ("host", host),
        ("x-amz-date", timestamp.to_owned()),
        (
            "x-amz-target",
            format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
        ),
    ]);
    if !credentials.session_token.is_empty() {
        values.insert("x-amz-security-token", credentials.session_token.clone());
    }
    let canonical_headers: String = values
        .iter()
        .map(|(key, value)| format!("{key}:{value}\n"))
        .collect();
    let signed_headers = values.keys().copied().collect::<Vec<_>>().join(";");
    let canonical = format!(
        "POST\n/\n\n{canonical_headers}\n{signed_headers}\n{:x}",
        Sha256::digest(body.as_bytes())
    );
    let date = &timestamp[..8];
    let scope = format!("{date}/{}/ecr/aws4_request", credentials.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{:x}",
        Sha256::digest(canonical.as_bytes())
    );
    let mut key = format!("AWS4{}", credentials.secret_access_key).into_bytes();
    for value in [date, &credentials.region, "ecr", "aws4_request", &to_sign] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
        mac.update(value.as_bytes());
        key = mac.finalize().into_bytes().to_vec();
    }
    let signature: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        let mut value = HeaderValue::from_str(&value).expect("validated ASCII header");
        if name == "x-amz-security-token" {
            value.set_sensitive(true);
        }
        headers.insert(reqwest::header::HeaderName::from_static(name), value);
    }
    let mut authorization = HeaderValue::from_str(&format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}", credentials.access_key_id,
    )).expect("validated ASCII access key");
    authorization.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, authorization);
    headers
}

/// Exchange AWS signing credentials for a login restricted to the returned ECR registry.
async fn ecr_credential(
    client: &Client,
    credentials: &AwsCredentials,
    endpoint: &str,
) -> Result<RegistryCredential, &'static str> {
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let response = client
        .post(endpoint)
        .headers(ecr_headers(
            credentials,
            &timestamp,
            "GetAuthorizationToken",
            "{}",
        ))
        .body("{}")
        .send()
        .await
        .map_err(|_| "Could not reach AWS ECR. Check the region and retry.")?;
    if !response.status().is_success() {
        return Err(
            "AWS ECR rejected the credentials. Check ecr:GetAuthorizationToken permission and session expiry.",
        );
    }
    /// One ECR authorization result with its registry endpoint.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Authorization {
        /// Base64-encoded `AWS:password` login issued by ECR.
        authorization_token: String,
        /// HTTPS registry endpoint checked against the requested account region.
        proxy_endpoint: String,
    }
    /// Response body from the ECR authorization-token operation.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Tokens {
        /// Registry-specific authorization results returned by AWS.
        authorization_data: Vec<Authorization>,
    }
    let tokens: Tokens = response_json(response).await?;
    let authorization = tokens
        .authorization_data
        .into_iter()
        .next()
        .ok_or("AWS ECR returned no authorization token")?;
    let registry = authorization
        .proxy_endpoint
        .strip_prefix("https://")
        .ok_or("AWS ECR returned an invalid registry")?;
    let account = registry.split('.').next().unwrap_or_default();
    if account.len() != 12
        || !account.bytes().all(|byte| byte.is_ascii_digit())
        || registry != ecr_registry(account, &credentials.region)
    {
        return Err("AWS ECR returned an unexpected registry");
    }
    let decoded = STANDARD
        .decode(&authorization.authorization_token)
        .map_err(|_| "AWS ECR returned an invalid token")?;
    let decoded = String::from_utf8(decoded).map_err(|_| "AWS ECR returned an invalid token")?;
    let secret = decoded
        .strip_prefix("AWS:")
        .filter(|secret| !secret.is_empty())
        .ok_or("AWS ECR returned an invalid token")?;
    Ok(RegistryCredential {
        registry: registry.into(),
        username: "AWS".into(),
        secret: secret.into(),
    })
}

/// Signed metadata requests use the same AWS account and region as the registry login.
pub async fn aws_repositories(
    credentials: &AwsCredentials,
    account: &str,
    page: u32,
    cursor: Option<&str>,
) -> Result<crate::oci::registry_catalog::RegistryRepositories, &'static str> {
    validate_aws(credentials)?;
    let host = ecr_api_host(&credentials.region);
    ecr_repositories(
        &client()?,
        credentials,
        account,
        page,
        cursor,
        &format!("https://{host}/"),
    )
    .await
}

/// Fetch one signed ECR catalog page and validate its account, names, and opaque cursor.
async fn ecr_repositories(
    client: &Client,
    credentials: &AwsCredentials,
    account: &str,
    page: u32,
    cursor: Option<&str>,
    endpoint: &str,
) -> Result<crate::oci::registry_catalog::RegistryRepositories, &'static str> {
    if !(1..=10000).contains(&page)
        || (page > 1 && cursor.is_none())
        || cursor.is_some_and(|value| {
            value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
        })
    {
        return Err("Invalid ECR repository cursor");
    }
    let mut body = serde_json::json!({ "registryId": account, "maxResults": 100 });
    if let Some(cursor) = cursor {
        body["nextToken"] = cursor.into();
    }
    let body = body.to_string();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let response = client
        .post(endpoint)
        .headers(ecr_headers(
            credentials,
            &timestamp,
            "DescribeRepositories",
            &body,
        ))
        .body(body)
        .send()
        .await
        .map_err(|_| "Could not reach AWS ECR. Retry repository discovery.")?;
    if !response.status().is_success() {
        return Err(
            "AWS ECR repository discovery failed. Check ecr:DescribeRepositories permission and session expiry.",
        );
    }
    /// Repository identity returned by the ECR catalog API.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        /// Owning AWS account, required to match the saved connection.
        registry_id: String,
        /// Repository path within that account's private registry.
        repository_name: String,
    }
    /// One ECR catalog page and its provider-issued continuation token.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repositories {
        /// Repository identities checked before being returned to the dashboard.
        repositories: Vec<Repository>,
        /// Opaque AWS cursor forwarded only to subsequent ECR catalog requests.
        next_token: Option<String>,
    }
    let result: Repositories = response_json(response).await?;
    let registry = ecr_registry(account, &credentials.region);
    let mut repositories = Vec::new();
    for repository in result.repositories {
        let reference = crate::oci::ImageReference::parse(&format!(
            "{registry}/{}:latest",
            repository.repository_name
        ))
        .map_err(|_| "AWS ECR returned an invalid repository")?;
        if repository.registry_id != account
            || reference.registry != registry
            || reference.repository != repository.repository_name
        {
            return Err("AWS ECR returned a repository from an unexpected account");
        }
        repositories.push(repository.repository_name);
    }
    let next_cursor = result.next_token.filter(|value| !value.is_empty());
    if next_cursor
        .as_ref()
        .is_some_and(|value| value.len() > 8192 || value.chars().any(char::is_control))
    {
        return Err("AWS ECR returned an invalid repository cursor");
    }
    Ok(crate::oci::registry_catalog::RegistryRepositories {
        repositories,
        next_page: if next_cursor.is_some() && page < 10000 {
            Some(page + 1)
        } else {
            None
        },
        next_cursor,
        listing_supported: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        routing::any,
    };

    fn aws() -> AwsCredentials {
        AwsCredentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "example-secret-key".into(),
            session_token: "session-token".into(),
            region: "ap-northeast-2".into(),
        }
    }

    async fn fixture(
        status: StatusCode,
        scopes: &'static str,
        body: String,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Receiver<(HeaderMap, String)>,
    ) {
        let (send, received) = tokio::sync::oneshot::channel();
        let send = std::sync::Arc::new(std::sync::Mutex::new(Some(send)));
        let app = Router::new().route(
            "/",
            any(move |headers: HeaderMap, request_body: String| {
                if let Some(send) = send.lock().unwrap().take() {
                    let _ = send.send((headers, request_body));
                }
                let body = body.clone();
                async move { (status, [("x-oauth-scopes", scopes)], body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}/"), task, received)
    }

    #[test]
    fn login_rejects_mixed_providers_unknown_fields_and_invalid_regions() {
        let value = serde_json::json!({ "provider": "aws", "accessKeyId": "AKIAIOSFODNN7EXAMPLE", "secretAccessKey": "key", "region": "ap-northeast-2" });
        assert!(serde_json::from_value::<Login>(value.clone()).is_ok());
        let mut mixed = value;
        mixed["token"] = "github-secret".into();
        assert!(serde_json::from_value::<Login>(mixed).is_err());
        assert!(
            serde_json::from_str::<Login>(
                r#"{"provider":"github","token":"x","region":"us-east-1"}"#
            )
            .is_err()
        );
        let mut credentials = aws();
        for region in [
            "../evil",
            "us-east-1.attacker.test",
            "",
            "us-east-",
            "us east 1",
        ] {
            credentials.region = region.into();
            assert!(validate_aws(&credentials).is_err(), "{region}");
        }
        for region in ["us-east-1", "us-gov-west-1", "cn-north-1", "ap-northeast-2"] {
            credentials.region = region.into();
            assert!(validate_aws(&credentials).is_ok(), "{region}");
        }
        assert!(validate_secret("token\nsecret", 16384).is_err());
    }

    #[test]
    fn ecr_signing_matches_independent_sigv4_vector_and_signs_session_token() {
        let headers = ecr_headers(&aws(), "20261021T010203Z", "GetAuthorizationToken", "{}");
        assert_eq!(
            headers["authorization"].to_str().unwrap(),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20261021/ap-northeast-2/ecr/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature=faec4f0a46610dc0b68f74ce8938deb72bc5391f1e19354f0461d98420ed7807"
        );
        assert!(headers["authorization"].is_sensitive());
        assert!(headers["x-amz-security-token"].is_sensitive());
        let mut credentials = aws();
        credentials.session_token.clear();
        credentials.region = "cn-north-1".into();
        let headers = ecr_headers(
            &credentials,
            "20261021T010203Z",
            "GetAuthorizationToken",
            "{}",
        );
        assert_eq!(headers["host"], "api.ecr.cn-north-1.amazonaws.com.cn");
        assert!(!headers.contains_key("x-amz-security-token"));
    }

    #[tokio::test]
    async fn github_verifies_identity_and_package_access_without_returning_token() {
        let (url, task, received) = fixture(
            StatusCode::OK,
            "read:user, read:packages",
            r#"{"login":"octocat"}"#.into(),
        )
        .await;
        let auth = github_auth(&Client::new(), "github-secret", &url)
            .await
            .unwrap();
        assert_eq!(
            received.await.unwrap().0["authorization"],
            "Bearer github-secret"
        );
        let connection = serde_json::to_value(auth.connection()).unwrap();
        assert_eq!(connection["account"], "octocat");
        assert_eq!(connection["registry"], "ghcr.io");
        assert!(!connection.to_string().contains("github-secret"));
        assert_eq!(
            auth.registry_credential().await.unwrap().secret,
            "github-secret"
        );
        task.abort();
        for (status, scopes) in [
            (StatusCode::OK, "read:user"),
            (StatusCode::OK, ""),
            (StatusCode::UNAUTHORIZED, "read:packages"),
        ] {
            let (url, task, _) = fixture(
                status,
                scopes,
                r#"{"login":"octocat","message":"echoed-github-secret"}"#.into(),
            )
            .await;
            let error = github_auth(&Client::new(), "github-secret", &url)
                .await
                .err()
                .unwrap();
            assert!(!error.contains("github-secret"));
            task.abort();
        }
    }

    #[tokio::test]
    async fn ecr_exchanges_keys_for_an_exact_host_login_and_rejects_untrusted_responses() {
        let token = STANDARD.encode("AWS:registry-password");
        for (endpoint, encoded, succeeds) in [
            (
                "https://123456789012.dkr.ecr.ap-northeast-2.amazonaws.com",
                token.as_str(),
                true,
            ),
            (
                "https://123456789012.dkr.ecr.us-east-1.amazonaws.com",
                token.as_str(),
                false,
            ),
            (
                "https://123456789012.dkr.ecr.ap-northeast-2.amazonaws.com.evil.test",
                token.as_str(),
                false,
            ),
            (
                "http://123456789012.dkr.ecr.ap-northeast-2.amazonaws.com",
                token.as_str(),
                false,
            ),
            (
                "https://123456789012.dkr.ecr.ap-northeast-2.amazonaws.com",
                "invalid-base64",
                false,
            ),
        ] {
            let body = serde_json::json!({ "authorizationData": [{ "authorizationToken": encoded, "proxyEndpoint": endpoint }] }).to_string();
            let (url, task, received) = fixture(StatusCode::OK, "", body).await;
            let result = ecr_credential(&Client::new(), &aws(), &url).await;
            assert_eq!(result.is_ok(), succeeds, "{endpoint}");
            let headers = received.await.unwrap().0;
            assert_eq!(headers["x-amz-security-token"], "session-token");
            if let Ok(credential) = result {
                assert_eq!(
                    credential.registry,
                    "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com"
                );
                assert_eq!(credential.username, "AWS");
                assert_eq!(credential.secret, "registry-password");
            }
            task.abort();
        }
    }
    #[tokio::test]
    async fn ecr_catalog_signs_pagination_and_rejects_foreign_repositories() {
        let body = serde_json::json!({ "repositories": [{ "registryId": "123456789012", "repositoryName": "team/app" }], "nextToken": "opaque+/=cursor" }).to_string();
        let (url, task, received) = fixture(StatusCode::OK, "", body).await;
        let page = ecr_repositories(
            &Client::new(),
            &aws(),
            "123456789012",
            2,
            Some("previous+/=cursor"),
            &url,
        )
        .await
        .unwrap();
        assert_eq!(page.repositories, ["team/app"]);
        assert_eq!(page.next_page, Some(3));
        assert_eq!(page.next_cursor.as_deref(), Some("opaque+/=cursor"));
        let (headers, body) = received.await.unwrap();
        assert_eq!(
            headers["x-amz-target"],
            "AmazonEC2ContainerRegistry_V20150921.DescribeRepositories"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({ "registryId": "123456789012", "maxResults": 100, "nextToken": "previous+/=cursor" })
        );
        let mut changed_body_headers =
            ecr_headers(&aws(), "20261021T010203Z", "DescribeRepositories", &body);
        assert_ne!(
            changed_body_headers.remove("authorization").unwrap(),
            ecr_headers(&aws(), "20261021T010203Z", "DescribeRepositories", "{}")["authorization"]
        );
        task.abort();
        let body = serde_json::json!({ "repositories": [{ "registryId": "999999999999", "repositoryName": "team/app" }] }).to_string();
        let (url, task, _) = fixture(StatusCode::OK, "", body).await;
        assert!(
            ecr_repositories(&Client::new(), &aws(), "123456789012", 1, None, &url)
                .await
                .is_err()
        );
        task.abort();
        let (url, task, _) = fixture(StatusCode::FORBIDDEN, "", "echoed-secret".into()).await;
        let error = ecr_repositories(&Client::new(), &aws(), "123456789012", 1, None, &url)
            .await
            .err()
            .unwrap();
        assert!(error.contains("ecr:DescribeRepositories"));
        assert!(!error.contains("echoed-secret"));
        assert!(
            ecr_repositories(&Client::new(), &aws(), "123456789012", 2, None, &url)
                .await
                .is_err()
        );
        task.abort();
    }
}
