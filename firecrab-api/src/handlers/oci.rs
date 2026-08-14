//! OCI inspect and import (`public-docs/oci.md`).
//!
//! `GET /api/oci/inspect` is metadata-only: it resolves a reference to the
//! manifest this host would pull so a wrong-architecture image is caught
//! before any download. `POST /api/oci/import` claims the derived alias and
//! runs the already-landed pipeline as an image-install job,
//! because REST requests time out at 10s. Poll `GET /api/oci/import/{alias}`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use firecrab_api_types::{ImageInstallResponse, OciImportRequest, OciInspectResponse};
use serde::Deserialize;

use crate::error::AppError;
use crate::extract::ValidatedJson;
use crate::image_install::Architecture;
use crate::oci::{
    ImageReference, ResolveError, claim_template_name, is_loopback_registry, resolve,
    run_oci_import, template_name_from_reference,
};
use crate::server::RequestId;
use crate::state::AppState;

/// Query string for [`inspect_oci_image`].
#[derive(Debug, Deserialize)]
pub struct InspectQuery {
    /// An image reference as typed at a `docker pull`, e.g. `nginx:1.27`.
    reference: String,
}

/// `GET /api/oci/inspect?reference=nginx:1.27`.
pub async fn inspect_oci_image(
    axum::Extension(request_id): axum::Extension<RequestId>,
    Query(query): Query<InspectQuery>,
) -> Result<Json<OciInspectResponse>, AppError> {
    let reference = parse_reference(&query.reference, request_id.0)?;
    let alias = template_name_from_reference(&reference)
        .map_err(|error| validation_reference(error.to_string(), request_id.0))?
        .alias;

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
            validation_reference(error.to_string(), request_id.0)
        })?;

    Ok(Json(OciInspectResponse {
        registry: reference.registry,
        repository: reference.repository,
        version: reference.version.as_str().to_owned(),
        immutable: reference.version.is_immutable(),
        digest: resolved.digest,
        architecture: resolved.architecture.as_str().to_owned(),
        single_platform: resolved.single_platform,
        alias,
    }))
}

/// `POST /api/oci/import` — claim the alias and start the import job.
pub async fn start_oci_import(
    State(state): State<AppState>,
    axum::Extension(request_id): axum::Extension<RequestId>,
    ValidatedJson(body): ValidatedJson<OciImportRequest>,
) -> Result<(StatusCode, Json<ImageInstallResponse>), AppError> {
    let reference = parse_reference(&body.reference, request_id.0)?;
    let named = match claim_template_name(&reference, &state.templates) {
        Ok(named) => named,
        Err(ResolveError::AliasCollision { .. }) => {
            return Err(AppError::conflict(
                "alias_collision",
                "derived alias collides with a catalog or installed image",
                request_id.0,
            ));
        }
        Err(error) => return Err(validation_reference(error.to_string(), request_id.0)),
    };

    let response = match state.oci_imports.begin_with(&named.alias, "import started") {
        Ok(snapshot) => snapshot,
        Err("running") => {
            return Err(AppError::conflict(
                "import_in_progress",
                "an import is already running for this image",
                request_id.0,
            ));
        }
        Err(_) => return Err(AppError::internal(request_id.0)),
    };

    let tracker = state.oci_imports.clone();
    let templates = (*state.templates).clone();
    let alias = named.alias.clone();
    tokio::spawn(async move {
        run_oci_import(tracker, templates, reference, alias).await;
    });

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// `GET /api/oci/import/{alias}` — latest import snapshot, including idle.
pub async fn get_oci_import(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    axum::Extension(_request_id): axum::Extension<RequestId>,
) -> Result<Json<ImageInstallResponse>, AppError> {
    Ok(Json(state.oci_imports.snapshot(&alias)))
}

fn parse_reference(reference: &str, request_id: uuid::Uuid) -> Result<ImageReference, AppError> {
    ImageReference::parse(reference)
        .map_err(|error| validation_reference(error.to_string(), request_id))
}

fn validation_reference(message: String, request_id: uuid::Uuid) -> AppError {
    AppError::validation([("reference".to_owned(), message)].into(), request_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::ValidatedJson;
    use crate::server::RequestId;
    use crate::state::AppState;
    use crate::templates::TemplateRegistry;
    use axum::Json;
    use axum::extract::{Path, Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use firecrab_api_types::{ImageInstallStatus, OciImportRequest};
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn empty_state(root: &std::path::Path) -> AppState {
        let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .unwrap()
    }

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

    /// Inspection resolves the host's manifest over plain HTTP on loopback,
    /// but remains metadata-only: config and layer blobs are import work.
    #[tokio::test]
    async fn a_loopback_registry_inspection_fetches_manifests_but_not_blobs() {
        use axum::http::header::CONTENT_TYPE;
        use axum::routing::get;

        let image_manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": format!("sha256:{}", "c".repeat(64)),
                "size": 3
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": format!("sha256:{}", "1".repeat(64)),
                "size": 5
            }]
        }))
        .expect("serialize image manifest fixture");
        let selected_digest = format!("sha256:{:x}", Sha256::digest(&image_manifest));
        let image_manifest_size = image_manifest.len();
        let image_manifest_route = format!("/v2/team/app/manifests/{selected_digest}");

        let index = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": selected_digest,
                "size": image_manifest_size,
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
        .expect("serialize image index fixture");

        let tag_requests = Arc::new(AtomicUsize::new(0));
        let selected_manifest_requests = Arc::new(AtomicUsize::new(0));
        let blob_requests = Arc::new(AtomicUsize::new(0));

        let app = axum::Router::new()
            .route(
                "/v2/team/app/manifests/v1",
                get({
                    let tag_requests = Arc::clone(&tag_requests);
                    move || {
                        let index = index.clone();
                        let tag_requests = Arc::clone(&tag_requests);
                        async move {
                            tag_requests.fetch_add(1, Ordering::SeqCst);
                            (
                                [(CONTENT_TYPE, "application/vnd.oci.image.index.v1+json")],
                                index,
                            )
                        }
                    }
                }),
            )
            .route(
                &image_manifest_route,
                get({
                    let selected_manifest_requests = Arc::clone(&selected_manifest_requests);
                    move || {
                        let image_manifest = image_manifest.clone();
                        let selected_manifest_requests = Arc::clone(&selected_manifest_requests);
                        async move {
                            selected_manifest_requests.fetch_add(1, Ordering::SeqCst);
                            (
                                [(CONTENT_TYPE, "application/vnd.oci.image.manifest.v1+json")],
                                image_manifest,
                            )
                        }
                    }
                }),
            )
            .route(
                "/v2/team/app/blobs/{digest}",
                get({
                    let blob_requests = Arc::clone(&blob_requests);
                    move || {
                        let blob_requests = Arc::clone(&blob_requests);
                        async move {
                            blob_requests.fetch_add(1, Ordering::SeqCst);
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
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

        assert_eq!(response.digest, selected_digest);
        assert_eq!(response.repository, "team/app");
        assert_eq!(response.architecture, Architecture::HOST.as_str());
        assert_eq!(response.alias, format!("127.0.0.1-{port}-team-app-v1"));
        assert!(!response.immutable);
        assert!(!response.single_platform);
        assert_eq!(tag_requests.load(Ordering::SeqCst), 1);
        assert_eq!(selected_manifest_requests.load(Ordering::SeqCst), 1);
        assert_eq!(blob_requests.load(Ordering::SeqCst), 0);
    }

    /// A parsable unused loopback reference is accepted immediately as a job.
    /// The pipeline is not waited on: REST would time out if it did.
    #[tokio::test]
    async fn start_oci_import_accepts_an_unused_loopback_reference() {
        let directory = tempfile::tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let (status, Json(response)) = start_oci_import(
            State(state.clone()),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            ValidatedJson(OciImportRequest {
                reference: "127.0.0.1:1/team/app:v1".to_owned(),
            }),
        )
        .await
        .expect("parsable unused reference must be accepted");

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response.alias, "127.0.0.1-1-team-app-v1");
        assert_eq!(response.status, ImageInstallStatus::Running);

        let Json(snapshot) = get_oci_import(
            State(state),
            Path("127.0.0.1-1-team-app-v1".to_owned()),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
        )
        .await
        .expect("accepted job must have a snapshot");
        assert_eq!(snapshot.alias, "127.0.0.1-1-team-app-v1");
        assert_eq!(snapshot.status, ImageInstallStatus::Running);
    }

    /// A parser rejection must not start a job: a typo should fail before
    /// any alias is claimed or any pipeline work is spawned.
    #[tokio::test]
    async fn start_oci_import_rejects_an_unparsable_reference_without_a_job() {
        let directory = tempfile::tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let error = start_oci_import(
            State(state.clone()),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            ValidatedJson(OciImportRequest {
                reference: "NGINX".to_owned(),
            }),
        )
        .await
        .unwrap_err();

        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "validation_failed");
        assert!(!state.oci_imports.is_running("NGINX"));
        assert_eq!(
            state.oci_imports.snapshot("NGINX").status,
            ImageInstallStatus::Idle
        );
    }

    #[tokio::test]
    async fn start_oci_import_rejects_a_catalog_alias_collision() {
        let directory = tempfile::tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let error = start_oci_import(
            State(state.clone()),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            ValidatedJson(OciImportRequest {
                reference: "ubuntu:26.04".to_owned(),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
        assert_eq!(
            state.oci_imports.snapshot("ubuntu-26.04").status,
            ImageInstallStatus::Idle
        );
    }

    #[tokio::test]
    async fn start_oci_import_rejects_a_second_job_for_the_same_alias() {
        let directory = tempfile::tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let body = OciImportRequest {
            reference: "127.0.0.1:1/team/app:v1".to_owned(),
        };
        let _started = start_oci_import(
            State(state.clone()),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            ValidatedJson(body.clone()),
        )
        .await
        .expect("first job");

        let error = start_oci_import(
            State(state),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            ValidatedJson(body),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_started_import_finishes_failed_when_the_registry_cannot_be_reached() {
        let directory = tempfile::tempdir().unwrap();
        let state = empty_state(directory.path()).await;
        let (_, Json(started)) = start_oci_import(
            State(state.clone()),
            axum::Extension(RequestId(uuid::Uuid::new_v4())),
            ValidatedJson(OciImportRequest {
                reference: "127.0.0.1:1/team/app:v1".to_owned(),
            }),
        )
        .await
        .expect("job accepted");

        let mut snapshot = state.oci_imports.snapshot(&started.alias);
        for _ in 0..80 {
            if snapshot.status == ImageInstallStatus::Failed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            snapshot = state.oci_imports.snapshot(&started.alias);
        }
        assert_eq!(snapshot.status, ImageInstallStatus::Failed);
        assert!(
            snapshot.log.contains("import failed"),
            "failed job must record why: {}",
            snapshot.log
        );
        assert!(
            !state
                .templates
                .image_root_path()
                .join(".oci/import")
                .join(&started.alias)
                .exists(),
            "scratch must be removed after failure"
        );
    }
}
