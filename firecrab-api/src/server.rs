use std::collections::HashSet;
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post};
use axum::{Extension, Router};
use thiserror::Error;
use tokio::sync::Semaphore;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers;
use crate::state::AppState;

const MAX_REQUEST_BODY: usize = 64 * 1024;
const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid FIRECRAB_BIND_ADDR: {0}")]
    InvalidBindAddress(String),
    #[error("non-loopback bind requires both authentication and TLS")]
    InsecureNonLoopbackBind,
    #[error("invalid FIRECRAB_ALLOWED_ORIGINS value: {0}")]
    InvalidOrigin(String),
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub bind_addr: SocketAddr,
    pub allowed_origins: Vec<HeaderValue>,
    pub max_concurrent_requests: usize,
    pub request_timeout: Duration,
    /// Directory of built dashboard assets to serve, or `None` to serve none
    /// (`public-docs/dashboard.md`). Set when an installed deploy
    /// should answer the browser itself instead of needing a Vite dev server
    /// alongside it; unset during development, where `npm run dev` proxies
    /// to this API.
    pub static_root: Option<PathBuf>,
}

impl HttpConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let bind = bind_addr_or_default(env::var("FIRECRAB_BIND_ADDR").ok());
        let authentication_enabled = env_flag("FIRECRAB_AUTHENTICATION_ENABLED");
        let tls_enabled = env_flag("FIRECRAB_TLS_ENABLED");
        let production = env::var("FIRECRAB_ENV").is_ok_and(|value| value == "production");
        let origins = env::var("FIRECRAB_ALLOWED_ORIGINS").unwrap_or_else(|_| {
            if production {
                String::new()
            } else {
                "http://localhost:8080".to_owned()
            }
        });

        let mut config = Self::from_values(&bind, &origins, authentication_enabled, tls_enabled)?;
        config.static_root = resolve_static_root(env::var("FIRECRAB_STATIC_ROOT").ok());
        Ok(config)
    }

    fn from_values(
        bind: &str,
        origins: &str,
        authentication_enabled: bool,
        tls_enabled: bool,
    ) -> Result<Self, ConfigError> {
        let bind_addr = SocketAddr::from_str(bind)
            .map_err(|_| ConfigError::InvalidBindAddress(bind.to_owned()))?;
        if !(is_loopback(bind_addr.ip()) || authentication_enabled && tls_enabled) {
            return Err(ConfigError::InsecureNonLoopbackBind);
        }

        let allowed_origins = origins
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(|origin| {
                HeaderValue::from_str(origin)
                    .map_err(|_| ConfigError::InvalidOrigin(origin.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            bind_addr,
            allowed_origins,
            max_concurrent_requests: 128,
            request_timeout: Duration::from_secs(10),
            static_root: None,
        })
    }
}

/// The dashboard assets to serve, if any. A configured directory that has no
/// `index.html` is treated as absent and logged rather than failing startup:
/// the API is still fully usable over HTTP, and a half-built `dist/` should
/// not stop VMs from being managed.
fn resolve_static_root(configured: Option<String>) -> Option<PathBuf> {
    let root = PathBuf::from(configured?);
    if root.join("index.html").is_file() {
        return Some(root);
    }
    tracing::warn!(
        path = %root.display(),
        "FIRECRAB_STATIC_ROOT has no index.html; serving the API only"
    );
    None
}

fn bind_addr_or_default(configured: Option<String>) -> String {
    configured.unwrap_or_else(|| "127.0.0.1:5523".to_owned())
}

fn env_flag(name: &str) -> bool {
    env::var(name).is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

#[derive(Clone)]
struct HttpPolicy {
    allowed_origins: HashSet<HeaderValue>,
}

#[derive(Clone)]
struct RequestLimits {
    permits: Arc<Semaphore>,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct RequestId(pub Uuid);

pub fn build_router(state: AppState, config: &HttpConfig) -> Router {
    let policy = HttpPolicy {
        allowed_origins: config.allowed_origins.iter().cloned().collect(),
    };
    let limits = RequestLimits {
        permits: Arc::new(Semaphore::new(config.max_concurrent_requests)),
        timeout: config.request_timeout,
    };
    let mut cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static("idempotency-key"),
        ]);
    if !config.allowed_origins.is_empty() {
        cors = cors.allow_origin(AllowOrigin::list(config.allowed_origins.clone()));
    }

    // The console WebSocket is intentionally its own sub-router: it must
    // stay open far longer than `enforce_limits`' request timeout allows,
    // and a request body limit means nothing for an upgraded connection.
    // Everything else keeps the full REST stack (CORS, body limit,
    // timeout/concurrency); both sub-routers still get origin enforcement
    // and request-id tagging, applied after the merge below.
    //
    // It also lives under a completely separate `/ws` prefix rather than
    // nested under `/api/vms/{id}/...`: dev-proxies (trunk's included) pick
    // HTTP vs. WebSocket handling per configured path prefix, not by
    // inspecting each request's Upgrade header, so an HTTP-proxied `/api`
    // prefix and a WS-proxied path can't overlap.
    let rest = Router::new()
        .route(
            "/api/vms",
            get(handlers::vms::list_vms).post(handlers::vms::create_vm_route),
        )
        .route(
            "/api/vms/{id}",
            get(handlers::vms::get_vm)
                .put(handlers::vms::update_vm)
                .delete(handlers::vms::delete_vm),
        )
        .route("/api/vms/{id}/start", post(handlers::vms::start_vm_request))
        .route("/api/vms/{id}/stop", post(handlers::vms::stop_vm))
        .route("/api/vms/{id}/log", get(handlers::vms::get_vm_log))
        .route(
            "/api/vms/{id}/ssh-key",
            get(handlers::vms::download_ssh_key),
        )
        .route(
            "/api/vms/{id}/ssh-host-key",
            get(handlers::vms::get_ssh_host_key),
        )
        .route(
            "/api/vms/{id}/ssh-host-key/check",
            get(handlers::vms::check_ssh_host_key),
        )
        .route("/api/network", get(handlers::network::get_network_info))
        .route("/api/host", get(handlers::network::get_host_status))
        // GET and POST share one path, matching this router's existing shape
        // for "read this resource / start work on it" pairs such as
        // `/api/images/{alias}/install`.
        .route(
            "/api/update",
            get(handlers::update::get_update_check).post(handlers::update::start_update),
        )
        .route(
            "/api/microregistry",
            get(handlers::microregistry::list_microregistry),
        )
        .route(
            "/api/microregistry/register",
            post(handlers::microregistry::start_microregistry_register),
        )
        .route(
            "/api/microregistry/register/{alias}",
            get(handlers::microregistry::get_microregistry_register),
        )
        .route(
            "/api/microregistry/docker-hub",
            get(handlers::microregistry::get_docker_hub_credential)
                .put(handlers::microregistry::put_docker_hub_credential)
                .delete(handlers::microregistry::delete_docker_hub_credential),
        )
        .route("/api/oci/inspect", get(handlers::oci::inspect_oci_image))
        .route("/api/oci/import", post(handlers::oci::start_oci_import))
        .route(
            "/api/oci/import/{alias}",
            get(handlers::oci::get_oci_import),
        )
        .route("/api/images", get(handlers::images::list_images))
        .route(
            "/api/images/{alias}",
            get(handlers::images::list_image_detail).delete(handlers::images::delete_image),
        )
        .route(
            "/api/images/{alias}/kernel",
            axum::routing::put(handlers::images::update_image_kernel),
        )
        .route("/api/kernels", get(handlers::kernels::list_kernels))
        .route(
            "/api/kernels/{version}",
            delete(handlers::kernels::delete_kernel),
        )
        .route(
            "/api/kernels/{version}/install",
            get(handlers::kernels::get_kernel_install)
                .post(handlers::kernels::start_kernel_install),
        )
        .route(
            "/api/images/{alias}/install",
            get(handlers::images::get_image_install).post(handlers::images::start_image_install),
        )
        .route(
            "/api/images/{alias}/package",
            get(handlers::images::get_image_package)
                .post(handlers::images::start_image_package)
                .delete(handlers::images::delete_staged_package),
        )
        .route(
            "/api/images/{alias}/bootstrap",
            post(handlers::bootstrap::start_bootstrap),
        )
        // Before the id-addressed route below only so the two read in the
        // order a reader meets them; axum matches the literal `bootstrap`
        // segment ahead of `{alias}` regardless of registration order, so
        // `GET /api/images/bootstrap` can never be taken for an alias named
        // "bootstrap".
        .route(
            "/api/images/bootstrap",
            get(handlers::bootstrap::get_active_bootstrap),
        )
        .route(
            "/api/images/bootstrap/{bootstrapId}",
            get(handlers::bootstrap::get_bootstrap).delete(handlers::bootstrap::cancel_bootstrap),
        )
        .route("/api/storage", get(handlers::storage::list_storage))
        .route(
            "/api/storage/devices",
            get(handlers::storage::list_storage_devices),
        )
        .route(
            "/api/micro-storages",
            get(handlers::micro_storages::list_micro_storages)
                .post(handlers::micro_storages::create_micro_storage),
        )
        .route(
            "/api/micro-storages/{id}",
            get(handlers::micro_storages::get_micro_storage)
                .delete(handlers::micro_storages::delete_micro_storage),
        )
        .route(
            "/api/vms/{id}/storage",
            axum::routing::put(handlers::vms::assign_vm_storage),
        )
        .route(
            "/api/micro-networks",
            get(handlers::micro_networks::list_micro_networks)
                .post(handlers::micro_networks::create_micro_network),
        )
        .route(
            "/api/micro-networks/{id}",
            get(handlers::micro_networks::get_micro_network)
                .patch(handlers::micro_networks::update_micro_network)
                .delete(handlers::micro_networks::delete_micro_network),
        )
        .route(
            "/api/shells",
            get(handlers::shells::list_shells).post(handlers::shells::create_shell),
        )
        .route(
            "/api/shells/{id}",
            get(handlers::shells::get_shell).delete(handlers::shells::delete_shell),
        )
        .route(
            "/api/shells/{id}/revisions",
            post(handlers::shells::create_shell_revision),
        )
        .route(
            "/api/shells/{id}/revisions/{revision_id}",
            get(handlers::shells::get_shell_revision),
        )
        .route(
            "/api/vms/{id}/shells",
            axum::routing::put(handlers::vms::update_vm_shells),
        )
        .route(
            "/api/vms/{id}/port-forwards",
            axum::routing::put(handlers::vms::update_vm_port_forwards),
        )
        .layer(cors)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY))
        .layer(middleware::from_fn_with_state(limits, enforce_limits));

    let console = Router::new().route("/ws/vms/{id}/console", get(handlers::console::console_ws));

    let app = rest
        .merge(console)
        // Unknown paths under the API/WebSocket prefixes answer with the JSON
        // error envelope even when the dashboard is served below, so a typo in
        // a client's URL never comes back as an HTML page.
        .route("/api/{*rest}", any(not_found))
        .route("/ws/{*rest}", any(not_found))
        .with_state(state);

    let app = match &config.static_root {
        // Anything else is the dashboard: a real file if it exists, otherwise
        // index.html, because the router's routes only exist in the browser
        // (a reload on /vms/<id> must not 404).
        Some(root) => app.fallback_service(
            ServeDir::new(root).fallback(ServeFile::new(root.join("index.html"))),
        ),
        None => app.fallback(not_found),
    };

    app.layer(middleware::from_fn_with_state(policy, enforce_origin))
        .layer(middleware::from_fn(assign_request_id))
}

async fn assign_request_id(mut request: Request, next: Next) -> Response {
    let request_id = RequestId(Uuid::new_v4());
    request.extensions_mut().insert(request_id);
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let started = std::time::Instant::now();

    let mut response = next.run(request).await;

    if let Ok(value) = HeaderValue::from_str(&request_id.0.to_string()) {
        response.headers_mut().insert(X_REQUEST_ID, value);
    }
    tracing::info!(
        request_id = %request_id.0,
        %method,
        path,
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "request"
    );
    response
}

/// Whether `origin` names the very address this request was sent to.
///
/// Browsers attach `Origin` to same-origin state-changing requests as well, so
/// once the dashboard is served by this API (`static_root`) every POST/DELETE it
/// makes carries one. Requiring that address to be listed would be a trap: it is
/// whatever the operator typed — `localhost`, a LAN IP, a proxy's hostname — and
/// getting it wrong turns every action in the UI into a `403` while GETs keep
/// working. A request whose `Origin` equals its own authority cannot have been
/// sent by another site, which is exactly what this check is defending against.
fn is_same_origin(origin: &HeaderValue, request: &Request) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    // HTTP/2 carries the authority in the URI; HTTP/1.1 in the Host header.
    let authority = request
        .uri()
        .authority()
        .map(|value| value.as_str())
        .or_else(|| {
            request
                .headers()
                .get(header::HOST)
                .and_then(|host| host.to_str().ok())
        });
    let Some((scheme, host)) = origin.split_once("://") else {
        // `null` (sandboxed frame, data: URL) and anything else without a
        // scheme is not this API's own page.
        return false;
    };
    // The scheme must be a web one: `chrome-extension://localhost:3000` parses
    // to the same authority as the dashboard, and an extension's page is not
    // it. http and https are both accepted for one authority on purpose — a
    // TLS-terminating proxy forwards `https://name` to this plaintext port, and
    // an attacker who could serve https on the very address the operator
    // reached would already own the connection.
    matches!(scheme, "http" | "https") && authority.is_some_and(|authority| host == authority)
}

async fn enforce_origin(
    State(policy): State<HttpPolicy>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let id = request_id(&request);
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !policy.allowed_origins.contains(origin)
        && !is_same_origin(origin, &request)
    {
        return Err(AppError::forbidden_origin(id));
    }
    Ok(next.run(request).await)
}

async fn enforce_limits(
    State(limits): State<RequestLimits>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let id = request_id(&request);
    let _permit = limits
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(id))?;

    tokio::time::timeout(limits.timeout, next.run(request))
        .await
        .map_err(|_| AppError::gateway_timeout(id))
}

async fn not_found(Extension(id): Extension<RequestId>) -> Response {
    AppError::not_found(id.0).into_response()
}

pub fn request_id(request: &Request) -> Uuid {
    request
        .extensions()
        .get::<RequestId>()
        .map_or_else(Uuid::new_v4, |id| id.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::assert_matches;

    #[test]
    fn defaults_are_loopback_only() {
        let config =
            HttpConfig::from_values("127.0.0.1:3000", "http://localhost:8080", false, false)
                .unwrap();
        assert!(config.bind_addr.ip().is_loopback());
    }

    #[test]
    fn rejects_insecure_non_loopback_bind() {
        let result = HttpConfig::from_values("0.0.0.0:3000", "", false, false);
        assert_matches!(result, Err(ConfigError::InsecureNonLoopbackBind));
    }

    #[test]
    fn bind_addr_defaults_when_unset() {
        assert_eq!(bind_addr_or_default(None), "127.0.0.1:5523");
    }

    #[test]
    fn bind_addr_honors_override() {
        assert_eq!(
            bind_addr_or_default(Some("0.0.0.0:9999".to_owned())),
            "0.0.0.0:9999"
        );
    }

    #[test]
    fn a_static_root_without_an_index_is_treated_as_absent() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_owned();
        assert_eq!(resolve_static_root(None), None);
        assert_eq!(
            resolve_static_root(Some(root.display().to_string())),
            None,
            "an unbuilt dist/ must not take over the router's fallback"
        );

        std::fs::write(root.join("index.html"), "<!doctype html>").unwrap();
        assert_eq!(
            resolve_static_root(Some(root.display().to_string())),
            Some(root)
        );
    }

    async fn test_state(root: &std::path::Path) -> AppState {
        let templates = crate::templates::TemplateRegistry::from_specs(root, std::iter::empty())
            .expect("empty template spec list should always verify");
        AppState::with_db_file(templates, root.join("state.db"))
            .await
            .expect("fresh temp db should open cleanly")
    }

    async fn get(app: &Router, path: &str) -> (axum::http::StatusCode, String) {
        use tower::ServiceExt;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn a_mutation_from_the_dashboards_own_origin_is_not_forbidden() {
        use tower::ServiceExt;

        let directory = tempfile::tempdir().unwrap();
        // The installed shape: dashboard served from this API, so nothing is
        // listed as a cross-origin caller.
        let config = HttpConfig::from_values("127.0.0.1:3000", "", false, false).unwrap();
        let app = build_router(test_state(directory.path()).await, &config);

        let post = |origin: &'static str| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(
                        Request::builder()
                            .method(Method::POST)
                            .uri("/api/vms")
                            .header(header::HOST, "localhost:3000")
                            .header(header::ORIGIN, origin)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from("{}"))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                response.status()
            }
        };

        // Same origin: reaches the handler (and fails validation there, which is
        // the point — it is not turned away at the door).
        assert_eq!(
            post("http://localhost:3000").await,
            axum::http::StatusCode::BAD_REQUEST
        );
        // A TLS-terminating proxy in front of this port sends the same
        // authority with an https scheme; that is still this dashboard.
        assert_eq!(
            post("https://localhost:3000").await,
            axum::http::StatusCode::BAD_REQUEST
        );

        for refused in [
            // Another site posting to this host.
            "http://evil.example",
            // Right authority, but an extension's page is not the dashboard.
            "chrome-extension://localhost:3000",
            // A sandboxed frame or a data: URL.
            "null",
            // Another port on the same host is a different origin.
            "http://localhost:8081",
        ] {
            assert_eq!(
                post(refused).await,
                axum::http::StatusCode::FORBIDDEN,
                "{refused} must not pass as the dashboard's own origin"
            );
        }
    }

    #[tokio::test]
    async fn without_a_static_root_every_unknown_path_is_a_json_404() {
        let directory = tempfile::tempdir().unwrap();
        let config = HttpConfig::from_values("127.0.0.1:3000", "", false, false).unwrap();
        let app = build_router(test_state(directory.path()).await, &config);

        for path in ["/", "/api/nope", "/dashboard"] {
            let (status, body) = get(&app, path).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{path}");
            assert!(
                body.contains("\"error\""),
                "{path} should answer JSON: {body}"
            );
        }
    }

    #[tokio::test]
    async fn with_a_static_root_the_dashboard_is_served_but_api_paths_still_answer_json() {
        let directory = tempfile::tempdir().unwrap();
        let assets = directory.path().join("dist");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(
            assets.join("index.html"),
            "<!doctype html><title>fc</title>",
        )
        .unwrap();
        std::fs::write(assets.join("app.js"), "console.log(1)").unwrap();

        let mut config = HttpConfig::from_values("127.0.0.1:3000", "", false, false).unwrap();
        config.static_root = Some(assets);
        let app = build_router(test_state(directory.path()).await, &config);

        let (status, body) = get(&app, "/").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("<title>fc</title>"), "{body}");

        let (status, body) = get(&app, "/app.js").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, "console.log(1)");

        // A client-side route the browser reloads on: no such file, so the
        // SPA entry point answers instead of a 404.
        let (status, body) = get(&app, "/vms/42").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("<title>fc</title>"), "{body}");

        // ... but a mistyped API path must not come back as HTML.
        for path in ["/api/nope", "/ws/nope"] {
            let (status, body) = get(&app, path).await;
            assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{path}");
            assert!(
                body.contains("\"error\""),
                "{path} should answer JSON: {body}"
            );
        }
    }
}
