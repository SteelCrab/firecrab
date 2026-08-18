use std::time::Duration;

use firecrab_api_types::HostStatusResponse;

/// Matches firecrab-api's own default (`bind_addr_or_default` in
/// firecrab-api/src/server.rs) — the API listens on 127.0.0.1:5523 unless
/// FIRECRAB_BIND_ADDR overrides it.
pub const DEFAULT_API_BASE: &str = "http://127.0.0.1:5523";

/// Distinguishes "never got an HTTP response" from "got one, and it was an
/// error" — callers (e.g. `status::collect`) report these differently.
#[derive(Debug)]
pub enum ApiError {
    /// Connection, TLS, timeout, or a response body that didn't parse.
    Unreachable(String),
    /// A well-formed but non-2xx response.
    Http { status: u16, body: String },
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unreachable(msg) => write!(f, "unreachable: {msg}"),
            ApiError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
        }
    }
}

/// `--api` flag > `FIRECRAB_API` env > `DEFAULT_API_BASE`. Trailing slash
/// stripped so callers can always do `format!("{base}/api/host")`.
pub fn resolve_api_base(flag: Option<&str>) -> String {
    if let Some(f) = flag {
        return f.trim_end_matches('/').to_owned();
    }
    if let Ok(env_val) = std::env::var("FIRECRAB_API") {
        if !env_val.is_empty() {
            return env_val.trim_end_matches('/').to_owned();
        }
    }
    DEFAULT_API_BASE.to_owned()
}

/// Thin blocking HTTP client for firecrab-api, used by `status`/other
/// subcommands that need live host data.
pub struct ApiClient {
    base: String,
    client: reqwest::blocking::Client,
}

impl ApiClient {
    /// `base` is used as-is (no re-validation) — pass it through
    /// [`resolve_api_base`] first. A 3s request timeout keeps `status`
    /// responsive when the API is down rather than hanging.
    pub fn new(base: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("reqwest client build");
        Self { base, client }
    }

    /// `GET {base}/api/host`, deserialized as `HostStatusResponse`.
    pub fn get_host_status(&self) -> Result<HostStatusResponse, ApiError> {
        let url = format!("{}/api/host", self.base);
        let resp = self.client.get(&url).send().map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(ApiError::Http { status: status.as_u16(), body });
        }
        resp.json::<HostStatusResponse>()
            .map_err(|e| ApiError::Unreachable(format!("bad response body: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_api_base_prefers_flag() {
        assert_eq!(resolve_api_base(Some("http://example.test:9000/")), "http://example.test:9000");
    }

    #[test]
    fn resolve_api_base_falls_back_to_default() {
        // No --api flag and (in this process) no FIRECRAB_API set.
        assert_eq!(resolve_api_base(None), DEFAULT_API_BASE);
    }

    #[test]
    fn unreachable_client_returns_unreachable_error() {
        // Port 1 is a reserved, never-listening port — connection refused
        // fast, well inside the 3s timeout, so this test stays quick.
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let err = client.get_host_status().unwrap_err();
        assert!(matches!(err, ApiError::Unreachable(_)));
    }
}
