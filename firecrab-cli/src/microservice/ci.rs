//! The public CI client uses bearer-only HTTPS, independent of the operator's mTLS profile.
use clap::{Args, Subcommand};
use firecrab_api_types::{
    CiDeployRequest, CiDeployStatus, CiServiceStatus, CreateDeployToken, CreatedDeployToken,
    DeployStrategy, DeployToken,
};
use reqwest::{blocking::Client, header::HeaderValue};
use serde::de::DeserializeOwned;
use std::{
    io::Read,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Public CI inputs do not read or select an operator host/mTLS profile.
#[derive(Debug, Args)]
pub struct DeployArgs {
    /// HTTPS service URL ending in /__firecrab/deploy. HTTP is accepted only on loopback.
    #[arg(long)]
    pub url: String,
    /// Fully qualified SHA-256 pin in the credential's exact repository.
    #[arg(long)]
    pub image: String,
    /// Read the token from this environment variable. Never pass a secret on the command line.
    #[arg(long, default_value = "FIRECRAB_DEPLOY_TOKEN")]
    pub token_env: String,
    /// Reuse after a lost response; a new UUID is printed when omitted.
    #[arg(long)]
    pub operation_id: Option<Uuid>,
    /// Reject an active release different from the one reviewed by this pipeline.
    #[arg(long)]
    pub expected_release: Option<Uuid>,
    /// Choose rolling or blue-green without changing any service configuration.
    #[arg(long,default_value="rolling",value_parser=["rolling","blue_green"])]
    pub strategy: String,
    /// Total waiting budget; reaching it never cancels accepted work.
    #[arg(long, default_value_t = 1800)]
    pub timeout: u64,
}

/// Operator-only credential administration through the normal authenticated API.
#[derive(Debug, Subcommand)]
pub enum Tokens {
    /// List safe metadata, including expired credentials that can be revoked.
    List,
    /// Issue a secret once for the service's current image repository.
    Create {
        /// Label identifying the pipeline that will hold this credential.
        #[arg(long)]
        name: String,
        /// Finite lifetime in seconds, defaulting to thirty days.
        #[arg(long,default_value_t=30*86400)]
        expires_in: u32,
    },
    /// Revoke one credential immediately without cancelling previously accepted work.
    Revoke {
        /// Public token UUID, not the secret bearer value.
        token: Uuid,
    },
}

/// Print plaintext only for explicit issuance; list/revoke never retrieve it.
pub(super) fn tokens(
    client: &crate::api_client::ApiClient,
    id: Uuid,
    command: Tokens,
) -> Result<(), super::Error> {
    let path = format!("/api/micro-services/{id}/deploy-tokens");
    match command {
        Tokens::List => super::print_json(&client.get::<Vec<DeployToken>>(&path)?),
        Tokens::Create { name, expires_in } => {
            super::print_json(&client.post::<_, CreatedDeployToken>(
                &path,
                &CreateDeployToken {
                    name,
                    expires_in_seconds: expires_in,
                },
            )?)
        }
        Tokens::Revoke { token } => {
            client.delete(&format!("{path}/{token}"))?;
        }
    }
    Ok(())
}

/// Static CI diagnostics intentionally exclude credentials, URLs and upstream body text.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Input or wire-contract guidance that contains no submitted values.
    #[error("{0}")]
    Invalid(&'static str),
    /// HTTP category only; the potentially sensitive remote body is discarded.
    #[error("Deploy endpoint returned HTTP {0}")]
    Http(u16),
    /// Connection/TLS failures can be retried only within the original deadline.
    #[error("Deploy endpoint is temporarily unavailable")]
    Unavailable,
    /// Waiting expired while the server may still own an accepted deployment.
    #[error("Deploy timed out; accepted work continues. Reuse the operation UUID to inspect it")]
    Timeout,
    /// The exact attempt failed, regardless of which release is currently serving.
    #[error("Deploy failed or was cancelled; inspect its exact operation with the operator API")]
    Failed,
}

/// Restrict remote access to HTTPS service paths and forbid credential-bearing URLs.
fn endpoint(value: &str) -> Result<String, Error> {
    let mut url = reqwest::Url::parse(value)
        .map_err(|_| Error::Invalid("Use an HTTPS service /__firecrab/deploy URL"))?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    let path = url.path().trim_end_matches('/');
    let local = path
        .strip_prefix("/api/ci/services/")
        .and_then(|p| p.strip_suffix("/deployments"))
        .is_some_and(|id| Uuid::parse_str(id).is_ok_and(|id| !id.is_nil()));
    if !matches!(url.scheme(), "http" | "https")
        || url.scheme() == "http" && !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !(path == "/__firecrab/deploy" || loopback && local)
        || value.bytes().any(|c| !c.is_ascii_graphic())
    {
        return Err(Error::Invalid(
            "Use an HTTPS service /__firecrab/deploy URL; HTTP is limited to loopback",
        ));
    }
    let path = path.to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

/// Reject mutable or control-bearing image references before constructing a request.
fn pinned(image: &str) -> bool {
    image.len() <= 1024
        && image.rsplit_once("@sha256:").is_some_and(|(repo, digest)| {
            repo.contains('/')
                && !repo.is_empty()
                && repo
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:/-".contains(&b))
                && digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// One no-redirect bearer connection and one deadline shared by all retries.
struct Connection {
    /// Client headers are marked sensitive and never logged by this module.
    client: Client,
    /// Validated service endpoint; suffixes are fixed paths or parsed UUIDs.
    url: String,
    /// Monotonic deadline that does not restart after a failed request.
    deadline: Instant,
}
impl Connection {
    /// Expiry returns failure without touching the accepted server operation.
    fn remaining(&self) -> Result<Duration, Error> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Error::Timeout)
    }
    /// Bound retry spacing by the remaining total waiting budget.
    fn pause(&self) -> Result<(), Error> {
        std::thread::sleep(self.remaining()?.min(Duration::from_secs(2)));
        Ok(())
    }
    /// Cap response size/time and report remote failures without copying their bodies.
    fn request<T: DeserializeOwned>(
        &self,
        suffix: &str,
        body: Option<&CiDeployRequest>,
    ) -> Result<T, Error> {
        let url = format!("{}{suffix}", self.url);
        let request = if let Some(body) = body {
            self.client.post(url).json(body)
        } else {
            self.client.get(url)
        };
        let mut response = request
            .timeout(self.remaining()?.min(Duration::from_secs(15)))
            .send()
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Unavailable)?;
        if bytes.len() > 65536 {
            return Err(Error::Invalid("Deploy response exceeds the size limit"));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::Invalid("Deploy endpoint returned an invalid response"))
    }
}

/// Retry availability/backpressure failures, never authorization or identity conflicts.
fn transient(error: &Error) -> bool {
    matches!(
        error,
        Error::Unavailable | Error::Http(429 | 502 | 503 | 504)
    )
}

/// Resume the exact attempt before considering submission, keeping one saved request after lost ACKs.
fn wait(
    connection: &Connection,
    image: &str,
    operation: Uuid,
    expected: Option<Uuid>,
    strategy: DeployStrategy,
) -> Result<CiDeployStatus, Error> {
    let suffix = format!("/{operation}");
    let mut body: Option<CiDeployRequest> = None;
    let mut last = None;
    loop {
        connection.remaining()?;
        let status = match connection.request::<CiDeployStatus>(&suffix, None) {
            Ok(status) => status,
            Err(Error::Http(404)) => {
                if body.is_none() {
                    let release = match expected {
                        Some(id) => id,
                        None => match connection.request::<CiServiceStatus>("", None) {
                            Ok(current) => current.release_id,
                            Err(error) if transient(&error) => {
                                connection.pause()?;
                                continue;
                            }
                            Err(error) => return Err(error),
                        },
                    };
                    if release.is_nil() {
                        return Err(Error::Invalid(
                            "Deploy endpoint returned an invalid release identity",
                        ));
                    }
                    body = Some(CiDeployRequest {
                        operation_id: operation,
                        expected_release_id: release,
                        image: image.into(),
                        strategy,
                    });
                }
                match connection.request::<CiDeployStatus>("", body.as_ref()) {
                    Ok(status) => status,
                    Err(error) if transient(&error) => {
                        connection.pause()?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) if transient(&error) => {
                connection.pause()?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if status.operation_id != operation
            || status.service_id.is_nil()
            || status.expected_release_id.is_nil()
            || status.image != image
            || status.strategy != strategy
            || expected
                .or(body.as_ref().map(|b| b.expected_release_id))
                .is_some_and(|id| id != status.expected_release_id)
        {
            return Err(Error::Invalid(
                "Operation UUID belongs to different Deploy inputs",
            ));
        }
        if last != Some(status.phase) {
            eprintln!("Deploy {operation}: {:?}", status.phase);
            last = Some(status.phase);
        }
        if status.completed {
            if !status.succeeded
                || !status
                    .digest
                    .as_deref()
                    .is_some_and(|d| pinned(&format!("registry.test/repo@{d}")))
            {
                return Err(Error::Failed);
            }
            return Ok(status);
        }
        connection.pause()?;
    }
}

/// Submit and wait without constructing the operator client or exposing its credentials.
pub fn deploy(args: DeployArgs) -> Result<(), Error> {
    let url = endpoint(&args.url)?;
    if !pinned(&args.image)
        || !(1..=86400).contains(&args.timeout)
        || args.operation_id.is_some_and(|id| id.is_nil())
        || args.expected_release.is_some_and(|id| id.is_nil())
    {
        return Err(Error::Invalid(
            "Use a digest-pinned image, non-nil UUIDs and a timeout of 1–86400 seconds",
        ));
    }
    let token = std::env::var(&args.token_env)
        .map_err(|_| Error::Invalid("The Deploy token environment variable is not set"))?;
    let valid = token
        .strip_prefix("fcd_")
        .and_then(|v| v.split_once('_'))
        .is_some_and(|(id, secret)| {
            id.len() == 32
                && Uuid::parse_str(id).is_ok()
                && secret.len() == 43
                && secret
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        });
    if !valid {
        return Err(Error::Invalid("A service Deploy token is required"));
    }
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::Invalid("Invalid Deploy token"))?;
    authorization.set_sensitive(true);
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, authorization);
    let client = Client::builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| Error::Unavailable)?;
    let operation = args.operation_id.unwrap_or_else(Uuid::new_v4);
    eprintln!("Deploy operation: {operation}");
    let connection = Connection {
        client,
        url,
        deadline: Instant::now() + Duration::from_secs(args.timeout),
    };
    let strategy = if args.strategy == "blue_green" {
        DeployStrategy::BlueGreen
    } else {
        DeployStrategy::Rolling
    };
    let result = wait(
        &connection,
        &args.image,
        operation,
        args.expected_release,
        strategy,
    )?;
    super::print_json(&result);
    Ok(())
}

#[cfg(test)]
mod tests;
