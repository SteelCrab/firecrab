use std::collections::HashSet;
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use thiserror::Error;
use tokio::sync::Semaphore;
use tower_http::cors::{AllowOrigin, CorsLayer};
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
}

impl HttpConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let bind = env::var("FIRECRAB_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_owned());
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

        Self::from_values(&bind, &origins, authentication_enabled, tls_enabled)
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
        })
    }
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
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
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
            get(handlers::vms::list_vms).post(handlers::vms::create_vm),
        )
        .route(
            "/api/vms/{id}",
            get(handlers::vms::get_vm)
                .put(handlers::vms::update_vm)
                .delete(handlers::vms::delete_vm),
        )
        .route("/api/vms/{id}/start", post(handlers::vms::start_vm))
        .route("/api/vms/{id}/stop", post(handlers::vms::stop_vm))
        .route("/api/vms/{id}/log", get(handlers::vms::get_vm_log))
        .route(
            "/api/vms/{id}/packages/update",
            post(handlers::packages::update_packages),
        )
        .route("/api/network", get(handlers::network::get_network_info))
        .route("/api/host", get(handlers::network::get_host_status))
        .layer(cors)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY))
        .layer(middleware::from_fn_with_state(limits, enforce_limits));

    let console = Router::new().route("/ws/vms/{id}/console", get(handlers::console::console_ws));

    rest.merge(console)
        .fallback(not_found)
        .with_state(state)
        .layer(middleware::from_fn_with_state(policy, enforce_origin))
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

async fn enforce_origin(
    State(policy): State<HttpPolicy>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let id = request_id(&request);
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !policy.allowed_origins.contains(origin)
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

    #[test]
    fn defaults_are_loopback_only() {
        let config =
            HttpConfig::from_values("127.0.0.1:3000", "http://localhost:8080", false, false)
                .unwrap();
        assert!(config.bind_addr.ip().is_loopback());
    }

    #[test]
    fn rejects_insecure_non_loopback_bind() {
        assert!(matches!(
            HttpConfig::from_values("0.0.0.0:3000", "", false, false),
            Err(ConfigError::InsecureNonLoopbackBind)
        ));
    }
}
