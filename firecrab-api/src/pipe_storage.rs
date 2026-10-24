//! AWS S3 connections and bounded, signed object operations.
use std::{collections::BTreeMap, time::Duration};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
};
use firecrab_api_types::{PipeStorageSpec, StorageCredentialRef, StorageObject, StorageObjectPage};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    persistence::{PersistenceError, Store},
    pipe_auth::{AwsCredentials, SavedAuth},
};

/// Single-object transfer limit. Transfers never buffer the entire object.
pub const MAX_OBJECT_BYTES: u64 = 512 * 1024 * 1024;
/// Total deadline for an object transfer, including its body.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_XML_BYTES: usize = 4 * 1024 * 1024;

/// Input-only access keys. Omit Debug to keep secrets out of diagnostics.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialsInput {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub session_token: String,
}

/// Secret-free configuration plus optional replacement signing material.
#[derive(Deserialize)]
#[serde(from = "StorageRequest")]
pub struct StorageInput {
    pub spec: PipeStorageSpec,
    pub credentials: Option<CredentialsInput>,
}

// Explicit fields keep unknown-field rejection reliable; serde flatten would discard them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StorageRequest {
    name: String,
    bucket: String,
    region: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    credential_ref: StorageCredentialRef,
    credentials: Option<CredentialsInput>,
}
impl From<StorageRequest> for StorageInput {
    fn from(request: StorageRequest) -> Self {
        Self {
            spec: PipeStorageSpec {
                name: request.name,
                bucket: request.bucket,
                region: request.region,
                prefix: request.prefix,
                credential_ref: request.credential_ref,
            },
            credentials: request.credentials,
        }
    }
}

/// Private database record; public responses use PipeStorageResponse instead.
#[derive(Clone, Deserialize, Serialize)]
pub struct StorageRecord {
    pub id: Uuid,
    pub spec: PipeStorageSpec,
    pub credentials: Option<AwsCredentials>,
}

/// Sanitized errors. Never retain upstream bodies, URLs, or signing material.
#[derive(Debug, Clone, Copy)]
pub struct StorageError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: &'static str,
}
impl StorageError {
    pub fn invalid(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "storage_validation",
            message,
        }
    }
    fn upstream(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "storage_upstream",
            message,
        }
    }
    fn transport(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            return Self {
                status: StatusCode::GATEWAY_TIMEOUT,
                code: "storage_timeout",
                message: "S3 operation timed out; check the network and retry",
            };
        }
        Self::upstream("Could not reach or read S3; check the network and retry")
    }
}

impl StorageInput {
    /// Validate before network work. Editing without credentials retains the private key.
    pub fn record(
        mut self,
        id: Uuid,
        previous: Option<&StorageRecord>,
    ) -> Result<StorageRecord, StorageError> {
        self.spec.name = self.spec.name.trim().to_owned();
        if self.spec.name.is_empty()
            || self.spec.name.chars().count() > 100
            || self.spec.name.chars().any(char::is_control)
        {
            return Err(StorageError::invalid(
                "Name must contain 1-100 characters without control characters",
            ));
        }
        let bucket = self.spec.bucket.as_bytes();
        if !(3..=63).contains(&bucket.len())
            || !bucket[0].is_ascii_alphanumeric()
            || !bucket[bucket.len() - 1].is_ascii_alphanumeric()
            || !bucket
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
            || self.spec.bucket.contains("..")
            || self.spec.bucket.parse::<std::net::Ipv4Addr>().is_ok()
            || self.spec.bucket.starts_with("xn--")
            || self.spec.bucket.starts_with("sthree-")
            || self.spec.bucket.starts_with("amzn-s3-demo-")
            || ["-s3alias", "--ol-s3", "--x-s3", "--table-s3"]
                .iter()
                .any(|s| self.spec.bucket.ends_with(s))
        {
            return Err(StorageError::invalid(
                "Use a general-purpose S3 bucket name with 3-63 lowercase letters, digits, periods or hyphens; IP addresses and consecutive periods are not allowed",
            ));
        }
        if !self.spec.prefix.is_empty() && !self.spec.prefix.ends_with('/') {
            self.spec.prefix.push('/');
        }
        validate_key(&self.spec.prefix, true)?;
        let credentials =
            match self.spec.credential_ref {
                StorageCredentialRef::PipeAuthAws => {
                    if self.credentials.is_some() {
                        return Err(StorageError::invalid(
                            "Omit credentials when referencing Auth AWS",
                        ));
                    }
                    None
                }
                StorageCredentialRef::Connection => match self.credentials {
                    Some(keys) => Some(AwsCredentials {
                        access_key_id: keys.access_key_id,
                        secret_access_key: keys.secret_access_key,
                        session_token: keys.session_token,
                        region: self.spec.region.clone(),
                    }),
                    None => Some(previous.and_then(|r| r.credentials.clone()).ok_or_else(
                        || StorageError::invalid("Provide AWS access keys for this connection"),
                    )?),
                },
            };
        crate::pipe_auth::validate_region(&self.spec.region).map_err(StorageError::invalid)?;
        if let Some(credentials) = &credentials {
            crate::pipe_auth::validate_aws(credentials).map_err(StorageError::invalid)?;
        }
        Ok(StorageRecord {
            id,
            spec: self.spec,
            credentials,
        })
    }
}

impl StorageRecord {
    /// Resolve a reference on the host; never exchange keys for an ECR token.
    pub fn signing_credentials(&self, store: &Store) -> Result<AwsCredentials, StorageError> {
        let mut credentials = match self.spec.credential_ref {
            StorageCredentialRef::Connection => self.credentials.clone().ok_or_else(|| {
                StorageError::invalid("This connection has no AWS credentials; edit the connection")
            })?,
            StorageCredentialRef::PipeAuthAws => store
                .pipe_auths()
                .map_err(|_| StorageError::upstream("Could not read Auth AWS"))?
                .into_iter()
                .find_map(|auth| match auth {
                    SavedAuth::Aws { credentials, .. } => Some(credentials),
                    _ => None,
                })
                .ok_or_else(|| {
                    StorageError::invalid(
                        "Connect AWS in Auth or select connection-specific access keys",
                    )
                })?,
        };
        credentials.region = self.spec.region.clone();
        crate::pipe_auth::validate_aws(&credentials).map_err(StorageError::invalid)?;
        Ok(credentials)
    }

    pub fn scope_key(&self, key: &str, allow_empty: bool) -> Result<(), StorageError> {
        validate_key(key, allow_empty)?;
        if !key.starts_with(&self.spec.prefix) {
            return Err(StorageError::invalid(
                "The key must stay inside the connection's configured prefix",
            ));
        }
        Ok(())
    }
}

fn validate_key(key: &str, allow_empty: bool) -> Result<(), StorageError> {
    if (!allow_empty && key.is_empty())
        || key.len() > 1024
        || key.chars().any(char::is_control)
        || key.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(StorageError::invalid(
            "Keys must contain 1-1024 UTF-8 bytes without control characters or . and .. path segments",
        ));
    }
    Ok(())
}

/// RFC 3986 encoding used by S3 SigV4; `/` is kept only in object paths.
fn encode(value: &str, keep_slashes: bool) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slashes && byte == b'/')
        {
            result.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(result, "%{byte:02X}").expect("writing to String");
        }
    }
    result
}

fn signed_headers(
    credentials: &AwsCredentials,
    method: &Method,
    host: &str,
    path: &str,
    query: &str,
    timestamp: &str,
    extra: &BTreeMap<&'static str, String>,
) -> HeaderMap {
    let mut values = extra.clone();
    values.insert("host", host.to_owned());
    values.insert("x-amz-date", timestamp.to_owned());
    values.insert("x-amz-content-sha256", "UNSIGNED-PAYLOAD".into());
    if !credentials.session_token.is_empty() {
        values.insert("x-amz-security-token", credentials.session_token.clone());
    }
    let canonical_headers: String = values
        .iter()
        .map(|(key, value)| format!("{key}:{}\n", value.trim()))
        .collect();
    let signed_headers = values.keys().copied().collect::<Vec<_>>().join(";");
    let canonical = format!(
        "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
    );
    let date = &timestamp[..8];
    let scope = format!("{date}/{}/s3/aws4_request", credentials.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{:x}",
        Sha256::digest(canonical.as_bytes())
    );
    let mut key = format!("AWS4{}", credentials.secret_access_key).into_bytes();
    for value in [date, &credentials.region, "s3", "aws4_request", &to_sign] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
        mac.update(value.as_bytes());
        key = mac.finalize().into_bytes().to_vec();
    }
    let signature: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        let mut value = HeaderValue::from_str(&value).expect("validated header");
        if name == "x-amz-security-token" {
            value.set_sensitive(true);
        }
        headers.insert(header::HeaderName::from_static(name), value);
    }
    let mut auth = HeaderValue::from_str(&format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}", credentials.access_key_id)).expect("validated access key");
    auth.set_sensitive(true);
    headers.insert(header::AUTHORIZATION, auth);
    headers
}

/// One connection's bounded HTTPS client. The endpoint is derived from bucket and region.
pub struct S3Client {
    client: Client,
    pub record: StorageRecord,
    credentials: AwsCredentials,
    origin: String,
    host: String,
    base_path: String,
}
impl S3Client {
    #[cfg(test)]
    pub(crate) fn with_test_origin(mut self, origin: &str) -> Self {
        let url = reqwest::Url::parse(origin).expect("test URL");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        self.client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TRANSFER_TIMEOUT)
            .build()
            .unwrap();
        self.origin = origin.into();
        self.host = format!("127.0.0.1:{}", url.port().unwrap());
        self
    }
    pub fn new(record: StorageRecord, credentials: AwsCredentials) -> Result<Self, StorageError> {
        let domain = if record.spec.region.starts_with("cn-") {
            "amazonaws.com.cn"
        } else {
            "amazonaws.com"
        };
        // Dotted buckets need path-style addressing to retain TLS hostname verification.
        let (host, base_path) = if record.spec.bucket.contains('.') {
            (
                format!("s3.{}.{}", record.spec.region, domain),
                format!("/{}", record.spec.bucket),
            )
        } else {
            (
                format!(
                    "{}.s3.{}.{}",
                    record.spec.bucket, record.spec.region, domain
                ),
                String::new(),
            )
        };
        let client = Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(15))
            .timeout(TRANSFER_TIMEOUT)
            .user_agent("firecrab-pipe-storage")
            .build()
            .map_err(|_| StorageError::upstream("Could not create S3 client"))?;
        Ok(Self {
            client,
            origin: format!("https://{host}"),
            host,
            record,
            credentials,
            base_path,
        })
    }

    async fn request(
        &self,
        method: Method,
        key: &str,
        query: Vec<(&str, String)>,
        extra: BTreeMap<&'static str, String>,
        body: Option<reqwest::Body>,
    ) -> Result<Response, StorageError> {
        let path = format!("{}/{}", self.base_path, encode(key, true));
        let mut pairs: Vec<_> = query
            .into_iter()
            .map(|(k, v)| (encode(k, false), encode(&v, false)))
            .collect();
        pairs.sort();
        let query = pairs
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let headers = signed_headers(
            &self.credentials,
            &method,
            &self.host,
            &path,
            &query,
            &timestamp,
            &extra,
        );
        let mut request = self
            .client
            .request(method.clone(), format!("{}{path}?{query}", self.origin))
            .headers(headers);
        if method != Method::PUT && (method != Method::GET || key.is_empty()) {
            request = request.timeout(Duration::from_secs(8));
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.map_err(StorageError::transport)?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(response_error(response, &self.record.spec.region).await)
    }

    /// Verify ListBucket on the exact prefix. No bucket enumeration or test writes.
    pub async fn verify(&self) -> Result<(), StorageError> {
        self.list(&self.record.spec.prefix, None, 1)
            .await
            .map(|_| ())
    }

    pub async fn list(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<StorageObjectPage, StorageError> {
        self.record.scope_key(prefix, true)?;
        if !(1..=1000).contains(&limit)
            || cursor
                .is_some_and(|v| v.is_empty() || v.len() > 8192 || v.chars().any(char::is_control))
        {
            return Err(StorageError::invalid(
                "Use a limit from 1 to 1000 and a valid continuation token",
            ));
        }
        let mut query = vec![
            ("list-type", "2".into()),
            ("prefix", prefix.into()),
            ("delimiter", "/".into()),
            ("encoding-type", "url".into()),
            ("max-keys", limit.to_string()),
        ];
        if let Some(cursor) = cursor {
            query.push(("continuation-token", cursor.into()));
        }
        let response = self
            .request(Method::GET, "", query, BTreeMap::new(), None)
            .await?;
        let xml = bounded_xml(response).await?;
        let page: ListResult = quick_xml::de::from_str(&xml)
            .map_err(|_| StorageError::upstream("S3 returned an invalid object listing"))?;
        if page.encoding_type != "url"
            || page
                .prefix
                .as_deref()
                .map(decode_key)
                .transpose()?
                .as_deref()
                != Some(prefix)
        {
            return Err(StorageError::upstream(
                "S3 returned an unexpected listing prefix or encoding",
            ));
        }
        if page.contents.len() + page.common_prefixes.len() > usize::from(limit)
            || page.is_truncated
                && page
                    .next_continuation_token
                    .as_deref()
                    .is_none_or(str::is_empty)
        {
            return Err(StorageError::upstream(
                "S3 returned invalid pagination metadata",
            ));
        }
        let mut objects = Vec::new();
        for object in page.contents {
            let key = decode_key(&object.key)?;
            self.record.scope_key(&key, false)?;
            if !key.starts_with(prefix) {
                return Err(StorageError::upstream(
                    "S3 returned an object outside the selected prefix",
                ));
            }
            objects.push(StorageObject {
                key,
                size: object.size,
                last_modified: object.last_modified,
            });
        }
        let mut prefixes = Vec::new();
        for entry in page.common_prefixes {
            let key = decode_key(&entry.prefix)?;
            self.record.scope_key(&key, false)?;
            if !key.starts_with(prefix) || !key.ends_with('/') {
                return Err(StorageError::upstream(
                    "S3 returned an invalid directory prefix",
                ));
            }
            prefixes.push(key);
        }
        Ok(StorageObjectPage {
            prefix: prefix.into(),
            objects,
            prefixes,
            next_cursor: if page.is_truncated {
                page.next_continuation_token
            } else {
                None
            },
        })
    }

    pub async fn upload(
        &self,
        key: &str,
        length: u64,
        overwrite: bool,
        body: Body,
    ) -> Result<(), StorageError> {
        self.record.scope_key(key, false)?;
        if length > MAX_OBJECT_BYTES {
            return Err(StorageError::invalid(
                "Object transfers are limited to 512 MiB",
            ));
        }
        let mut headers = BTreeMap::from([
            ("content-length", length.to_string()),
            ("content-type", "application/octet-stream".into()),
        ]);
        if !overwrite {
            headers.insert("if-none-match", "*".into());
        }
        let stream = body.into_data_stream().scan(0_u64, move |total, item| {
            let item = item
                .map_err(|_| std::io::Error::other("Could not read upload body"))
                .and_then(|bytes| {
                    *total += bytes.len() as u64;
                    if *total > length {
                        Err(std::io::Error::other("Upload exceeds Content-Length"))
                    } else {
                        Ok(bytes)
                    }
                });
            std::future::ready(Some(item))
        });
        self.request(
            Method::PUT,
            key,
            vec![],
            headers,
            Some(reqwest::Body::wrap_stream(stream)),
        )
        .await?;
        Ok(())
    }

    pub async fn download(&self, key: &str) -> Result<Response, StorageError> {
        self.record.scope_key(key, false)?;
        let response = self
            .request(Method::GET, key, vec![], BTreeMap::new(), None)
            .await?;
        if response
            .content_length()
            .is_none_or(|n| n > MAX_OBJECT_BYTES)
        {
            return Err(StorageError::invalid(
                "Object transfers require a known size of at most 512 MiB",
            ));
        }
        Ok(response)
    }

    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.record.scope_key(key, false)?;
        self.request(Method::DELETE, key, vec![], BTreeMap::new(), None)
            .await?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListResult {
    encoding_type: String,
    prefix: Option<String>,
    #[serde(default)]
    is_truncated: bool,
    next_continuation_token: Option<String>,
    #[serde(default, rename = "Contents")]
    contents: Vec<ListObject>,
    #[serde(default, rename = "CommonPrefixes")]
    common_prefixes: Vec<ListPrefix>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListObject {
    key: String,
    size: u64,
    last_modified: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListPrefix {
    prefix: String,
}

fn decode_key(value: &str) -> Result<String, StorageError> {
    // S3 listings can encode a space as '+'. Replace it before decoding %2B.
    let value = value.replace('+', " ");
    percent_encoding::percent_decode_str(&value)
        .decode_utf8()
        .map(|s| s.into_owned())
        .map_err(|_| StorageError::upstream("S3 returned an invalid UTF-8 key"))
}

async fn bounded_xml(mut response: Response) -> Result<String, StorageError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(StorageError::transport)? {
        if bytes.len() + chunk.len() > MAX_XML_BYTES {
            return Err(StorageError::upstream(
                "S3 response exceeded the metadata limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| StorageError::upstream("S3 returned an invalid UTF-8 response"))
}

async fn response_error(response: Response, region: &str) -> StorageError {
    let status = response.status();
    if response
        .headers()
        .get("x-amz-bucket-region")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != region)
        && (status.is_redirection() || status == StatusCode::BAD_REQUEST)
    {
        return StorageError::invalid(
            "S3 bucket region does not match; edit the connection region",
        );
    }
    #[derive(Deserialize)]
    struct ErrorCode {
        #[serde(rename = "Code")]
        code: String,
    }
    let code = bounded_xml(response)
        .await
        .ok()
        .and_then(|xml| quick_xml::de::from_str::<ErrorCode>(&xml).ok())
        .map(|v| v.code);
    let (status, code, message) = match code.as_deref() {
        Some("ExpiredToken" | "InvalidToken" | "TokenRefreshRequired") => (
            StatusCode::UNAUTHORIZED,
            "storage_credentials_expired",
            "AWS session expired or is invalid; replace the credentials",
        ),
        Some("InvalidAccessKeyId" | "SignatureDoesNotMatch") => (
            StatusCode::UNAUTHORIZED,
            "storage_auth_failed",
            "AWS rejected the signing credentials; check the keys, region and host clock",
        ),
        Some("NoSuchBucket") => (
            StatusCode::NOT_FOUND,
            "storage_bucket_missing",
            "The S3 bucket does not exist or is not accessible",
        ),
        Some("NoSuchKey") => (
            StatusCode::NOT_FOUND,
            "storage_object_missing",
            "The selected S3 object does not exist",
        ),
        Some("AuthorizationHeaderMalformed" | "PermanentRedirect") => (
            StatusCode::BAD_REQUEST,
            "storage_region_mismatch",
            "S3 bucket region does not match; edit the connection region",
        ),
        _ if status == StatusCode::FORBIDDEN => (
            StatusCode::FORBIDDEN,
            "storage_access_denied",
            "S3 denied this operation; check the bucket/prefix IAM policy and credential expiry",
        ),
        _ if status == StatusCode::PRECONDITION_FAILED || status == StatusCode::CONFLICT => (
            StatusCode::CONFLICT,
            "storage_object_conflict",
            "The key exists or changed during the operation; refresh and choose overwrite explicitly",
        ),
        _ if status == StatusCode::NOT_FOUND => (
            status,
            "storage_not_found",
            "The S3 bucket or object was not found",
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "storage_upstream",
            "S3 rejected the operation; check permissions, region and bucket settings",
        ),
    };
    StorageError {
        status,
        code,
        message,
    }
}

/// Build guest-only environment values from a durable deployment's VM ownership.
/// Values never enter VmRecord.env or the public deployment snapshot.
pub fn environment_for_vm(
    store: &Store,
    vm_id: Uuid,
) -> Result<BTreeMap<String, String>, StorageError> {
    for service in store
        .list_micro_services()
        .map_err(|_| StorageError::upstream("Could not read service bindings"))?
    {
        let Some(deployment) = store
            .service_deployment(service.id)
            .map_err(|_| StorageError::upstream("Could not read service deployment"))?
        else {
            continue;
        };
        if !deployment
            .instances
            .iter()
            .any(|instance| instance.vm_id == vm_id)
        {
            continue;
        }
        let Some(id) = deployment.spec.storage_connection_id else {
            return Ok(BTreeMap::new());
        };
        let record = store
            .pipe_storage(id)
            .map_err(|_| StorageError::upstream("Could not read storage connection"))?
            .ok_or_else(|| StorageError::invalid("The service's storage connection is missing"))?;
        let credentials = record.signing_credentials(store)?;
        return Ok(BTreeMap::from([
            ("AWS_ACCESS_KEY_ID".into(), credentials.access_key_id),
            (
                "AWS_SECRET_ACCESS_KEY".into(),
                credentials.secret_access_key,
            ),
            ("AWS_SESSION_TOKEN".into(), credentials.session_token),
            ("AWS_REGION".into(), record.spec.region.clone()),
            ("AWS_DEFAULT_REGION".into(), record.spec.region),
            ("FIRECRAB_S3_BUCKET".into(), record.spec.bucket),
            ("FIRECRAB_S3_PREFIX".into(), record.spec.prefix),
        ]));
    }
    Ok(BTreeMap::new())
}

/// Convert database corruption without including private JSON in the error.
pub fn corrupt(id: Uuid) -> PersistenceError {
    PersistenceError::CorruptRecord {
        id: id.to_string(),
        reason: "invalid PipeStorage configuration".into(),
    }
}

#[cfg(test)]
#[path = "pipe_storage_tests.rs"]
pub(crate) mod tests;
