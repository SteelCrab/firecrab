use std::time::Duration;

use firecrab_api_types::HostStatusResponse;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Default timeout for API reads and mutations.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Short timeout used only by the host status probe.
const HOST_STATUS_TIMEOUT: Duration = Duration::from_secs(3);
/// Registry authentication and manifest retries have a 60s API deadline.
const OCI_INSPECT_TIMEOUT: Duration = Duration::from_secs(65);

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

/// Resolves `--api` > `FIRECRAB_API` > `--host` > current host > loopback.
pub fn resolve_api_base(
    flag: Option<&str>,
    requested_host: Option<&str>,
) -> Result<String, crate::hosts::Error> {
    if let Some(flag) = flag {
        return crate::hosts::normalize_url(flag);
    }
    if let Ok(environment) = std::env::var("FIRECRAB_API")
        && !environment.is_empty()
    {
        return crate::hosts::normalize_url(&environment);
    }
    let selected = crate::hosts::selected(requested_host)?;
    match selected {
        Some(host) => Ok(host.url),
        None => default_api_base(),
    }
}

pub(crate) fn default_api_base() -> Result<String, crate::hosts::Error> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if let Some(base) =
        crate::micromanager::local_api_base().map_err(crate::hosts::Error::LocalConfiguration)?
    {
        return Ok(base);
    }
    Ok(DEFAULT_API_BASE.to_owned())
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
    pub fn try_new(base: String) -> Result<Self, ApiError> {
        let mut builder = reqwest::blocking::Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if base.starts_with("https://") {
            let config =
                crate::tls::client_config().map_err(|e| ApiError::Unreachable(e.to_string()))?;
            builder = builder.use_preconfigured_tls(config);
        }
        let client = builder
            .build()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Ok(Self { base, client })
    }

    #[cfg(test)]
    pub fn new(base: String) -> Self {
        Self::try_new(base).expect("test API client")
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
            .timeout(if path == "/api/oci/inspect" {
                OCI_INSPECT_TIMEOUT
            } else {
                DEFAULT_REQUEST_TIMEOUT
            })
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
            .timeout(if path.starts_with("/api/pipe/ai/") {
                self.mutation_timeout().max(OCI_INSPECT_TIMEOUT)
            } else {
                self.mutation_timeout()
            })
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
            .timeout(self.mutation_timeout())
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends a body-less `POST` with an `Idempotency-Key` header when `key`
    /// is set, so a retried request can be recognized by the API.
    pub fn post_empty_idempotent<T: DeserializeOwned>(
        &self,
        path: &str,
        key: Option<&str>,
    ) -> Result<T, ApiError> {
        let mut request = self.client.post(self.url(path));
        if let Some(key) = key {
            request = request.header(firecrab_api_types::IDEMPOTENCY_KEY_HEADER, key);
        }
        let resp = request
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends `body` as JSON with `PATCH` and deserializes any successful 2xx
    /// response body.
    pub fn patch<B, T>(&self, path: &str, body: &B) -> Result<T, ApiError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let resp = self
            .client
            .patch(self.url(path))
            .json(body)
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends a `DELETE` request and deserializes the successful response
    /// body, for endpoints that answer with the resource they changed.
    pub fn delete_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let resp = self
            .client
            .delete(self.url(path))
            .timeout(if path.starts_with("/api/pipe/ai/") {
                self.mutation_timeout().max(OCI_INSPECT_TIMEOUT)
            } else {
                self.mutation_timeout()
            })
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::decode_json(resp)
    }

    /// Sends `body` as JSON with `PUT` and deserializes any successful 2xx
    /// response body.
    pub fn put<B, T>(&self, path: &str, body: &B) -> Result<T, ApiError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let resp = self
            .client
            .put(self.url(path))
            .json(body)
            .timeout(if path.starts_with("/api/pipe/ai/") {
                self.mutation_timeout().max(OCI_INSPECT_TIMEOUT)
            } else {
                self.mutation_timeout()
            })
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
            .timeout(self.mutation_timeout())
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::ensure_success(resp).map(|_| ())
    }

    /// Send a DELETE with URL-encoded identity guards and accept an empty 2xx response.
    pub fn delete_query<Q: Serialize + ?Sized>(
        &self,
        path: &str,
        query: &Q,
    ) -> Result<(), ApiError> {
        let resp = self
            .client
            .delete(self.url(path))
            .query(query)
            .timeout(self.mutation_timeout())
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::ensure_success(resp).map(|_| ())
    }

    fn mutation_timeout(&self) -> Duration {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if reqwest::Url::parse(self.base_url())
            .ok()
            .is_some_and(|url| matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
        {
            // Includes a cold microManager boot before the request is forwarded.
            return Duration::from_secs(360);
        }
        DEFAULT_REQUEST_TIMEOUT
    }

    /// Stream a known-size file with a deadline matching PipeStorage's transfer routes.
    pub fn upload_object(&self, path: &str, file: &std::path::Path) -> Result<(), ApiError> {
        let input = std::fs::File::open(file)
            .map_err(|_| ApiError::Unreachable("Could not open upload file".into()))?;
        let metadata = input
            .metadata()
            .map_err(|_| ApiError::Unreachable("Could not inspect upload file".into()))?;
        if !metadata.is_file() || metadata.len() > 512 * 1024 * 1024 {
            return Err(ApiError::Unreachable(
                "Upload must be a regular file of at most 512 MiB".into(),
            ));
        }
        let response = self
            .client
            .put(self.url(path))
            .timeout(Duration::from_secs(125))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(reqwest::blocking::Body::sized(input, metadata.len()))
            .send()
            .map_err(|_| {
                ApiError::Unreachable(
                    "S3 upload interrupted; inspect the object before retrying".into(),
                )
            })?;
        Self::ensure_success(response).map(|_| ())
    }

    /// Keep interrupted downloads private and leave existing destination files intact.
    pub fn download_object(
        &self,
        path: &str,
        file: &std::path::Path,
        overwrite: bool,
    ) -> Result<(), ApiError> {
        let response = self
            .client
            .get(self.url(path))
            .timeout(Duration::from_secs(125))
            .send()
            .map_err(|_| ApiError::Unreachable("Could not start S3 download".into()))?;
        let mut response = Self::ensure_success(response)?;
        let length = response
            .content_length()
            .filter(|n| *n <= 512 * 1024 * 1024)
            .ok_or_else(|| ApiError::Unreachable("Invalid S3 download size".into()))?;
        if file.exists() && !overwrite {
            return Err(ApiError::Unreachable(
                "Destination exists; use --overwrite explicitly".into(),
            ));
        }
        let parent = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| ApiError::Unreachable("Could not create download file".into()))?;
        let copied = std::io::copy(&mut response, &mut temporary).map_err(|_| {
            ApiError::Unreachable("S3 download interrupted; destination was not replaced".into())
        })?;
        if copied != length {
            return Err(ApiError::Unreachable(
                "Incomplete S3 download; destination was not replaced".into(),
            ));
        }
        let result = if overwrite {
            temporary.persist(file)
        } else {
            temporary.persist_noclobber(file)
        };
        result.map_err(|_| {
            ApiError::Unreachable("Could not save download; destination was not replaced".into())
        })?;
        Ok(())
    }

    /// Sends `GET` and returns the raw successful body. Used for attachments
    /// such as `GET /api/vms/{id}/ssh-key` that are not JSON.
    #[cfg(test)]
    pub fn get_bytes(&self, path: &str) -> Result<Vec<u8>, ApiError> {
        let resp = self
            .client
            .get(self.url(path))
            .send()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Self::ensure_success(resp)?
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|e| ApiError::Unreachable(format!("bad response body: {e}")))
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

    #[test]
    fn interrupted_s3_download_keeps_the_existing_destination_and_removes_temporary_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = ApiClient::new(format!("http://{}", listener.local_addr().unwrap()));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let count = stream.read(&mut request).unwrap();
            assert!(count > 0);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial",
                )
                .unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("destination.bin");
        std::fs::write(&file, b"original bytes").unwrap();
        assert!(
            client
                .download_object("/api/pipe/storage/id/object?key=team/file", &file, true)
                .is_err()
        );
        server.join().unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"original bytes");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

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
        serve_once_after(status, content_type, body, Duration::ZERO)
    }

    fn serve_once_after(
        status: &str,
        content_type: Option<&str>,
        body: &str,
        delay: Duration,
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
            thread::sleep(delay);

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
            resolve_api_base(Some("http://example.test:9000/"), Some("ignored")).unwrap(),
            "http://example.test:9000"
        );
    }

    #[test]
    fn resolve_api_base_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK against the other test in this
        // file that touches FIRECRAB_API; no other test reads it.
        unsafe { std::env::remove_var("FIRECRAB_API") };
        unsafe { std::env::set_var("FIRECRAB_CONFIG_DIR", directory.path()) };
        assert_eq!(resolve_api_base(None, None).unwrap(), DEFAULT_API_BASE);
        unsafe { std::env::remove_var("FIRECRAB_CONFIG_DIR") };
    }

    #[test]
    fn resolve_api_base_reads_env_var_when_no_flag() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by ENV_LOCK — see the note above.
        unsafe { std::env::set_var("FIRECRAB_API", "http://env-example.test:1234/") };
        let result = resolve_api_base(None, Some("ignored")).unwrap();
        unsafe { std::env::remove_var("FIRECRAB_API") };
        assert_eq!(result, "http://env-example.test:1234");
    }

    #[test]
    fn resolve_api_base_reads_an_explicit_and_current_saved_host() {
        let _guard = ENV_LOCK.lock().unwrap();
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("config.toml"),
            r#"
current_host = "lab"

[hosts.lab]
url = "http://lab.example:5523"

[hosts.prod]
url = "https://prod.example:5523/"
"#,
        )
        .unwrap();
        // SAFETY: ENV_LOCK serializes every environment-changing test here.
        unsafe {
            std::env::remove_var("FIRECRAB_API");
            std::env::set_var("FIRECRAB_CONFIG_DIR", directory.path());
        }

        assert_eq!(
            resolve_api_base(None, Some("prod")).unwrap(),
            "https://prod.example:5523"
        );
        assert_eq!(
            resolve_api_base(None, None).unwrap(),
            "http://lab.example:5523"
        );

        unsafe { std::env::remove_var("FIRECRAB_CONFIG_DIR") };
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
    fn registry_inspection_can_outlast_the_normal_cli_deadline() {
        let (base, _requests, server) = serve_once_after(
            "200 OK",
            Some("application/json"),
            r#"{"value":"inspected"}"#,
            DEFAULT_REQUEST_TIMEOUT + Duration::from_secs(1),
        );
        let response: FixtureResponse = ApiClient::new(base)
            .get_query("/api/oci/inspect", &[("reference", "alpine:3.21")])
            .unwrap();
        assert_eq!(response.value, "inspected");
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
    fn put_sends_path_and_json_body() {
        let (base, requests, server) =
            serve_once("200 OK", Some("application/json"), r#"{"value":"updated"}"#);
        let client = ApiClient::new(base);
        let body = FixtureRequest {
            name: "example",
            count: 3,
        };

        let response: FixtureResponse = client.put("/api/vms/123", &body).unwrap();

        assert_eq!(response.value, "updated");
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "PUT");
        assert_eq!(request.path, "/api/vms/123");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({"name": "example", "count": 3})
        );
        server.join().unwrap();
    }

    #[test]
    fn get_bytes_returns_the_raw_body() {
        let (base, requests, server) = serve_once(
            "200 OK",
            Some("application/x-pem-file"),
            "-----BEGIN KEY-----\n",
        );
        let client = ApiClient::new(base);

        let body = client.get_bytes("/api/vms/123/ssh-key").unwrap();

        assert_eq!(body, b"-----BEGIN KEY-----\n");
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/vms/123/ssh-key");
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
