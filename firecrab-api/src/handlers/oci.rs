//! `GET /api/oci/inspect`: answer whether an OCI image can run on this host,
//! before anything is downloaded.
//!
//! Import itself (layer merge, whiteouts, guest enablement) is separate work;
//! this endpoint only resolves a reference to the manifest this host would
//! pull, so a wrong-architecture image is caught at the point a user types it.

use axum::Json;
use axum::extract::Query;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::image_install::Architecture;
use crate::oci::{ImageReference, is_loopback_registry, resolve};
use crate::server::RequestId;

/// Query string for [`inspect_oci_image`].
#[derive(Debug, Deserialize)]
pub struct InspectQuery {
    /// An image reference as typed at a `docker pull`, e.g. `nginx:1.27`.
    reference: String,
}

/// What this host resolved the reference to.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectResponse {
    /// Registry host the reference resolved to.
    pub registry: String,
    /// Repository path, with Docker Hub's implicit `library/` filled in.
    pub repository: String,
    /// The tag or digest that was resolved.
    pub version: String,
    /// Whether that version can never be repointed at other content.
    pub immutable: bool,
    /// Digest of the manifest this host would pull.
    pub digest: String,
    /// The architecture that manifest runs, as a registry label.
    pub architecture: String,
    /// True when the registry answered with a manifest rather than an index,
    /// so no per-platform selection took place.
    pub single_platform: bool,
}

/// `GET /api/oci/inspect?reference=nginx:1.27`.
pub async fn inspect_oci_image(
    axum::Extension(request_id): axum::Extension<RequestId>,
    Query(query): Query<InspectQuery>,
) -> Result<Json<InspectResponse>, AppError> {
    let reference = ImageReference::parse(&query.reference).map_err(|error| {
        AppError::validation(
            [("reference".to_owned(), error.to_string())].into(),
            request_id.0,
        )
    })?;

    let insecure = is_loopback_registry(&reference.registry);
    let resolved = resolve(&reference, Architecture::HOST, insecure)
        .await
        .map_err(|error| {
            tracing::warn!(
                request_id = %request_id.0,
                reference = %query.reference,
                error = %error,
                "OCI image inspection failed"
            );
            AppError::validation(
                [("reference".to_owned(), error.to_string())].into(),
                request_id.0,
            )
        })?;

    Ok(Json(InspectResponse {
        registry: reference.registry,
        repository: reference.repository,
        version: reference.version.as_str().to_owned(),
        immutable: reference.version.is_immutable(),
        digest: resolved.digest,
        architecture: resolved.architecture.as_str().to_owned(),
        single_platform: resolved.single_platform,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::RequestId;
    use axum::extract::Query;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    /// A reference the parser rejects must never reach the network: a typo
    /// should answer immediately, not after a registry timeout.
    #[tokio::test]
    async fn an_unparsable_reference_fails_before_any_request() {
        let error = inspect_oci_image(
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            Query(InspectQuery {
                reference: "NGINX".to_owned(),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
    }

    /// A registry on loopback is how a local development registry runs, and
    /// it serves plain HTTP. Requiring TLS there makes it unusable.
    #[tokio::test]
    async fn a_loopback_registry_is_reached_over_plain_http() {
        use axum::routing::get;

        let app = axum::Router::new().route(
            "/v2/team/app/manifests/v1",
            get(|| async {
                axum::Json(serde_json::json!({
                    "schemaVersion": 2,
                    "manifests": [{
                        "digest": "sha256:selected",
                        "size": 12,
                        "platform": {
                            "architecture": if Architecture::HOST == Architecture::Aarch64 {
                                "arm64"
                            } else {
                                "amd64"
                            },
                            "os": "linux"
                        }
                    }]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let response = inspect_oci_image(
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            Query(InspectQuery {
                reference: format!("127.0.0.1:{port}/team/app:v1"),
            }),
        )
        .await
        .expect("a loopback registry must be reachable")
        .0;

        assert_eq!(response.digest, "sha256:selected");
        assert_eq!(response.repository, "team/app");
        assert_eq!(response.architecture, Architecture::HOST.as_str());
        assert!(!response.immutable);
        assert!(!response.single_platform);
    }
}
