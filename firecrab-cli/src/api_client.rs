use std::time::Duration;

use firecrab_api_types::HostStatusResponse;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Default timeout for API reads and mutations.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Short timeout used only by the host status probe.
const HOST_STATUS_TIMEOUT: Duration = Duration::from_secs(3);

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

impl std::error::Error for ApiError {}

/// `--api` flag > `FIRECRAB_API` env > `DEFAULT_API_BASE`. Trailing slash
/// stripped so callers can always do `format!("{base}/api/host")`.
pub fn resolve_api_base(flag: Option<&str>) -> String {
    if let Some(f) = flag {
        return f.trim_end_matches('/').to_owned();
    }
    if let Ok(env_val) = std::env::var("FIRECRAB_API")
        && !env_val.is_empty()
    {
        return env_val.trim_end_matches('/').to_owned();
    }
    DEFAULT_API_BASE.to_owned()
}

/// Thin blocking HTTP client for firecrab-api, used by `status`/other
/// subcommands that need live host data.
pub struct ApiClient {
    /// Resolved API origin without a trailing slash.
    base: String,
    /// Shared blocking client with the normal API request timeout.
    client: reqwest::blocking::Client,
}

impl ApiClient {
    /// `base` is used as-is (no re-validation) — pass it through
    /// [`resolve_api_base`] first. The 15s default is longer than the API's
    /// own 10s request deadline so mutation requests can receive the API's
    /// response instead of timing out first. [`Self::get_host_status`] keeps
    /// its shorter 3s timeout so `status` stays responsive when the API is
    /// down.
    pub fn new(base: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client build");
        Self { base, client }
    }

    /// Resolved API origin without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// `GET {base}/api/host`, deserialized as `HostStatusResponse`.
    pub fn get_host_status(&self) -> Result<HostStatusResponse, ApiError> {
        let resp = self
            .client
            .get(self.url("/api/host"))
            .timeout(HOST_STATUS_TIMEOUT)
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends a JSON `GET` request to an API path such as `/api/vms` and
    /// deserializes any successful 2xx response body.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let resp = self
            .client
            .get(self.url(path))
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends a JSON `GET` request with URL-encoded query parameters and
    /// deserializes any successful 2xx response body.
    pub fn get_query<Q, T>(&self, path: &str, query: &Q) -> Result<T, ApiError>
    where
        Q: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let resp = self
            .client
            .get(self.url(path))
            .query(query)
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends `body` as JSON to an API path and deserializes any successful
    /// 2xx response body.
    pub fn post<B, T>(&self, path: &str, body: &B) -> Result<T, ApiError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let resp = self
            .client
            .post(self.url(path))
            .json(body)
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends a body-less `POST` request to an API path and deserializes any
    /// successful 2xx response body.
    pub fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let resp = self
            .client
            .post(self.url(path))
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends a `DELETE` request to an API path. Any 2xx response, including
    /// the API's usual `204 No Content`, is successful.
    pub fn delete(&self, path: &str) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(self.url(path))
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::ensure_success(resp).map(|_| ())
    }

    /// Joins the resolved origin and an absolute API path.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Checks the status and decodes a successful JSON response.
    fn decode_json<T: DeserializeOwned>(resp: reqwest::blocking::Response) -> Result<T, ApiError> {
        Self::ensure_success(resp)?
            .json::<T>()
            .map_err(|e| ApiError::Unreachable(format!("bad response body: {e}")))
    }

    /// Returns successful responses unchanged and captures error bodies.
    fn ensure_success(
        resp: reqwest::blocking::Response,
    ) -> Result<reqwest::blocking::Response, ApiError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        let body = resp
            .text()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Err(ApiError::Http {
            status: status.as_u16(),
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        path: String,
        body: Vec<u8>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct FixtureResponse {
        value: String,
    }

    #[derive(Debug, Serialize)]
    struct FixtureRequest<'a> {
        name: &'a str,
        count: u8,
    }

    /// Serializes the two tests below that read/write the real
    /// `FIRECRAB_API` process env var — `std::env::set_var` is process-wide,
    /// so without this lock they'd race under `cargo test`'s parallel
    /// runner (one could observe the other's value mid-test).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn serve_once(
        status: &str,
        content_type: Option<&str>,
        body: &str,
    ) -> (String, Receiver<CapturedRequest>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let response_body = body.as_bytes().to_vec();
        let status = status.to_owned();
        let content_type = content_type.map(str::to_owned);
        let (tx, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            tx.send(request).unwrap();

            let mut headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                response_body.len()
            );
            if let Some(content_type) = content_type {
                headers.push_str(&format!("Content-Type: {content_type}\r\n"));
            }
            headers.push_str("\r\n");
            stream.write_all(headers.as_bytes()).unwrap();
            stream.write_all(&response_body).unwrap();
        });
        (format!("http://{address}"), rx, handle)
    }

    fn read_request(stream: &mut TcpStream) -> CapturedRequest {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "connection closed before HTTP headers");
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break position + 4;
            }
        };

        let (method, path, content_length) = {
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
            let mut request_line = headers.lines().next().unwrap().split_whitespace();
            let method = request_line.next().unwrap().to_owned();
            let path = request_line.next().unwrap().to_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            (method, path, content_length)
        };
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "connection closed before HTTP body");
            bytes.extend_from_slice(&chunk[..read]);
        }

        CapturedRequest {
            method,
            path,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    #[test]
    fn resolve_api_base_prefers_flag() {
        assert_eq!(
            resolve_api_base(Some("http://example.test:9000/")),
            "http://example.test:9000"
        );
    }

    #[test]
    fn resolve_api_base_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK against the other test in this
        // file that touches FIRECRAB_API; no other test reads it.
        unsafe { std::env::remove_var("FIRECRAB_API") };
        assert_eq!(resolve_api_base(None), DEFAULT_API_BASE);
    }

    #[test]
    fn resolve_api_base_reads_env_var_when_no_flag() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note above.
        unsafe { std::env::set_var("FIRECRAB_API", "http://env-example.test:1234/") };
        let result = resolve_api_base(None);
        unsafe { std::env::remove_var("FIRECRAB_API") };
        assert_eq!(result, "http://env-example.test:1234");
    }

    #[test]
    fn unreachable_client_returns_unreachable_error() {
        // Port 1 is a reserved, never-listening port — connection refused
        // fast, well inside either request timeout, so this test stays quick.
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let err = client.get_host_status().unwrap_err();
        assert!(matches!(err, ApiError::Unreachable(_)));
    }

    #[test]
    fn get_sends_path_and_deserializes_json() {
        let (base, requests, server) =
            serve_once("200 OK", Some("application/json"), r#"{"value":"listed"}"#);
        let client = ApiClient::new(base);

        let response: FixtureResponse = client.get("/api/vms?state=running").unwrap();

        assert_eq!(
            response,
            FixtureResponse {
                value: "listed".to_owned()
            }
        );
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/vms?state=running");
        assert!(request.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn get_query_url_encodes_the_reference() {
        let (base, requests, server) = serve_once(
            "200 OK",
            Some("application/json"),
            r#"{"value":"inspected"}"#,
        );
        let client = ApiClient::new(base);

        let response: FixtureResponse = client
            .get_query(
                "/api/oci/inspect",
                &[("reference", "ghcr.io/org/app:v1@sha256:abc")],
            )
            .unwrap();

        assert_eq!(response.value, "inspected");
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(
            request.path,
            "/api/oci/inspect?reference=ghcr.io%2Forg%2Fapp%3Av1%40sha256%3Aabc"
        );
        assert!(request.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn post_sends_path_and_json_body_and_accepts_created() {
        let (base, requests, server) = serve_once(
            "201 Created",
            Some("application/json"),
            r#"{"value":"created"}"#,
        );
        let client = ApiClient::new(base);
        let body = FixtureRequest {
            name: "example",
            count: 2,
        };

        let response: FixtureResponse = client.post("/api/vms", &body).unwrap();

        assert_eq!(response.value, "created");
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/vms");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({"name": "example", "count": 2})
        );
        server.join().unwrap();
    }

    #[test]
    fn post_empty_sends_no_body_and_deserializes_json() {
        let (base, requests, server) =
            serve_once("200 OK", Some("application/json"), r#"{"value":"started"}"#);
        let client = ApiClient::new(base);

        let response: FixtureResponse = client.post_empty("/api/vms/123/start").unwrap();

        assert_eq!(response.value, "started");
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/vms/123/start");
        assert!(request.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn delete_accepts_no_content_and_sends_expected_request() {
        let (base, requests, server) = serve_once("204 No Content", None, "");
        let client = ApiClient::new(base);

        client.delete("/api/micro-networks/123").unwrap();

        let request = requests.recv().unwrap();
        assert_eq!(request.method, "DELETE");
        assert_eq!(request.path, "/api/micro-networks/123");
        assert!(request.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn non_success_response_preserves_body_verbatim() {
        let body = "  validation failed\n{not-json}\n";
        let (base, _requests, server) = serve_once("422 Unprocessable Entity", None, body);
        let client = ApiClient::new(base);

        let err = client.get::<FixtureResponse>("/api/vms").unwrap_err();

        match err {
            ApiError::Http {
                status,
                body: actual,
            } => {
                assert_eq!(status, 422);
                assert_eq!(actual, body);
            }
            ApiError::Unreachable(message) => panic!("unexpected transport error: {message}"),
        }
        server.join().unwrap();
    }

    #[test]
    fn invalid_success_json_is_an_unreachable_error() {
        let (base, _requests, server) = serve_once("200 OK", Some("application/json"), "not-json");
        let client = ApiClient::new(base);

        let err = client.get::<FixtureResponse>("/api/vms").unwrap_err();

        match err {
            ApiError::Unreachable(message) => {
                assert!(message.starts_with("bad response body:"));
            }
            ApiError::Http { status, body } => {
                panic!("unexpected HTTP error {status}: {body}")
            }
        }
        server.join().unwrap();
    }

    #[test]
    fn api_error_display_unreachable() {
        let err = ApiError::Unreachable("connection refused".to_owned());
        assert_eq!(err.to_string(), "unreachable: connection refused");
    }

    #[test]
    fn api_error_display_http() {
        let err = ApiError::Http {
            status: 500,
            body: "boom".to_owned(),
        };
        assert_eq!(err.to_string(), "HTTP 500: boom");
    }
}
