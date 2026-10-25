//! Provider calls stay on the server. Redirects and raw provider error bodies are never forwarded.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::{Client, Method, RequestBuilder, Url};
use ring::hmac;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Connection, MAX_SECRETS, Provider, SecretError, validate_name, validate_value};

/// Maximum decoded body accepted from an external provider response.
const MAX_RESPONSE: usize = 20 * 1024 * 1024;

/// Validates provider settings and required credentials without contacting the store.
pub(crate) fn validate(connection: &Connection) -> Result<(), SecretError> {
    let settings = &connection.summary.settings;
    if connection.summary.name.trim().is_empty() || connection.summary.name.len() > 100 {
        return Err(SecretError::Invalid(
            "connection name must contain 1-100 characters",
        ));
    }
    let required: &[&str] = match connection.summary.provider {
        Provider::Firecrab => {
            base_url(&settings.base_url)?;
            &[]
        }
        Provider::Infisical => {
            base_url(&settings.base_url)?;
            if settings.project_id.is_empty()
                || settings.environment.is_empty()
                || !settings.secret_path.starts_with('/')
            {
                return Err(SecretError::Invalid(
                    "Infisical needs a project ID, environment and absolute secret path",
                ));
            }
            &["token"]
        }
        Provider::AwsSecretsManager => {
            if settings.region.is_empty()
                || settings.region.len() > 40
                || !settings
                    .region
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(SecretError::Invalid("AWS region is invalid"));
            }
            &["accessKeyId", "secretAccessKey"]
        }
    };
    if required.iter().any(|name| {
        connection
            .credentials
            .get(*name)
            .is_none_or(String::is_empty)
    }) {
        return Err(SecretError::Invalid(
            "required connection credentials are missing",
        ));
    }
    if connection
        .credentials
        .values()
        .any(|value| value.len() > 8192 || value.contains(['\r', '\n']))
    {
        return Err(SecretError::Invalid("connection credentials are invalid"));
    }
    if [
        &settings.base_url,
        &settings.project_id,
        &settings.environment,
        &settings.secret_path,
        &settings.prefix,
    ]
    .iter()
    .any(|value| value.len() > 2048)
    {
        return Err(SecretError::Invalid("connection settings are too long"));
    }
    Ok(())
}

/// Builds a client with an eight-second request limit and redirects disabled.
fn client() -> Result<Client, SecretError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| SecretError::Provider)
}

/// Accepts HTTPS or loopback HTTP URLs without embedded credentials, queries, or fragments.
fn base_url(value: &str) -> Result<Url, SecretError> {
    let mut url = Url::parse(value).map_err(|_| SecretError::Invalid("base URL is invalid"))?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SecretError::Invalid(
            "use an HTTPS base URL; HTTP is allowed only for loopback",
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

/// Resolves a provider endpoint beneath its validated base URL.
fn endpoint(connection: &Connection, path: &str) -> Result<Url, SecretError> {
    base_url(&connection.summary.settings.base_url)?
        .join(path)
        .map_err(|_| SecretError::Provider)
}

/// Adds a saved bearer token for Infisical or an authenticating Firecrab proxy.
fn authorized(connection: &Connection, request: RequestBuilder) -> RequestBuilder {
    match connection
        .credentials
        .get("token")
        .filter(|value| !value.is_empty())
    {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

/// Encodes a JSON body and its content type consistently for provider requests.
fn json_body(request: RequestBuilder, body: &Value) -> RequestBuilder {
    request
        .header("content-type", "application/json")
        .body(body.to_string())
}

/// Decodes bounded successful JSON without exposing raw provider error bodies.
async fn response_json(request: RequestBuilder) -> Result<Value, SecretError> {
    let response = request.send().await.map_err(|_| SecretError::Provider)?;
    if !response.status().is_success() {
        return Err(SecretError::Provider);
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE as u64)
    {
        return Err(SecretError::Provider);
    }
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.try_next().await.map_err(|_| SecretError::Provider)? {
        if body.len() + chunk.len() > MAX_RESPONSE {
            return Err(SecretError::Provider);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| SecretError::Provider)
}

/// Loads a bounded name/value snapshot and validates each transferable entry.
pub(crate) async fn list(connection: &Connection) -> Result<BTreeMap<String, String>, SecretError> {
    validate(connection)?;
    let client = client()?;
    let values = match connection.summary.provider {
        Provider::Infisical => list_infisical(&client, connection).await?,
        Provider::Firecrab => list_firecrab(&client, connection).await?,
        Provider::AwsSecretsManager => list_aws(&client, connection).await?,
    };
    if values.len() > MAX_SECRETS {
        return Err(SecretError::Invalid(
            "a transfer supports at most 1000 secrets",
        ));
    }
    for (name, value) in &values {
        validate_name(name)?;
        validate_value(value)?;
    }
    Ok(values)
}

/// Reads one Infisical scope with raw references and rejects hidden or duplicate values.
async fn list_infisical(
    client: &Client,
    connection: &Connection,
) -> Result<BTreeMap<String, String>, SecretError> {
    let settings = &connection.summary.settings;
    let url = endpoint(connection, "api/v4/secrets")?;
    let result = response_json(authorized(
        connection,
        client.get(url).query(&[
            ("projectId", settings.project_id.as_str()),
            ("environment", settings.environment.as_str()),
            ("secretPath", settings.secret_path.as_str()),
            ("recursive", "false"),
            ("viewSecretValue", "true"),
            ("expandSecretReferences", "false"),
        ]),
    ))
    .await?;
    let items = result["secrets"].as_array().ok_or(SecretError::Provider)?;
    let mut values = BTreeMap::new();
    for item in items {
        // Hidden values are not empty values. Refuse a snapshot we cannot compare safely.
        if item["secretValueHidden"].as_bool() == Some(true) {
            return Err(SecretError::Invalid(
                "Infisical permission to read secret values is required",
            ));
        }
        let name = item["secretKey"].as_str().ok_or(SecretError::Provider)?;
        let value = item["secretValue"].as_str().ok_or(SecretError::Provider)?;
        if values.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(SecretError::Provider);
        }
    }
    Ok(values)
}

/// Fetches bounded Firecrab metadata without using a reveal endpoint.
async fn firecrab_summaries(
    client: &Client,
    connection: &Connection,
) -> Result<Vec<Value>, SecretError> {
    let result = response_json(authorized(
        connection,
        client.get(endpoint(connection, "api/secrets")?),
    ))
    .await?;
    let items = result.as_array().ok_or(SecretError::Provider)?;
    if items.len() > MAX_SECRETS {
        return Err(SecretError::Invalid(
            "a transfer supports at most 1000 secrets",
        ));
    }
    Ok(items.clone())
}

/// Resolves Firecrab metadata to explicit reveals with at most eight concurrent reads.
async fn list_firecrab(
    client: &Client,
    connection: &Connection,
) -> Result<BTreeMap<String, String>, SecretError> {
    let items = firecrab_summaries(client, connection).await?;
    let pairs: Vec<(String, String)> = stream::iter(items)
        .map(|item| async move {
            let id = item["id"].as_str().ok_or(SecretError::Provider)?;
            let id = uuid::Uuid::parse_str(id).map_err(|_| SecretError::Provider)?;
            let name = item["name"]
                .as_str()
                .ok_or(SecretError::Provider)?
                .to_owned();
            let result = response_json(authorized(
                connection,
                client.post(endpoint(connection, &format!("api/secrets/{id}/reveal"))?),
            ))
            .await?;
            Ok::<_, SecretError>((
                name,
                result["value"]
                    .as_str()
                    .ok_or(SecretError::Provider)?
                    .to_owned(),
            ))
        })
        .buffer_unordered(8)
        .try_collect()
        .await?;
    let count = pairs.len();
    let values: BTreeMap<_, _> = pairs.into_iter().collect();
    if count != values.len() {
        return Err(SecretError::Provider);
    }
    Ok(values)
}

/// Follows AWS listing pages and retrieves each SecretString within the configured scope.
async fn list_aws(
    client: &Client,
    connection: &Connection,
) -> Result<BTreeMap<String, String>, SecretError> {
    let mut names = Vec::new();
    let mut next = None;
    loop {
        let mut body = json!({"MaxResults":100});
        if !connection.summary.settings.prefix.is_empty() {
            body["Filters"] = json!([{"Key":"name","Values":[connection.summary.settings.prefix]}]);
        }
        if let Some(token) = &next {
            body["NextToken"] = json!(token);
        }
        let result = aws_request(client, connection, "ListSecrets", &body).await?;
        for item in result["SecretList"]
            .as_array()
            .ok_or(SecretError::Provider)?
        {
            names.push(
                item["Name"]
                    .as_str()
                    .ok_or(SecretError::Provider)?
                    .to_owned(),
            );
        }
        if names.len() > MAX_SECRETS {
            return Err(SecretError::Invalid(
                "a transfer supports at most 1000 secrets; narrow the AWS name prefix",
            ));
        }
        let token = result["NextToken"].as_str().map(str::to_owned);
        if token.is_none() {
            break;
        }
        if token == next {
            return Err(SecretError::Provider);
        }
        next = token;
    }
    stream::iter(names)
        .map(|name| async move {
            let result = aws_request(
                client,
                connection,
                "GetSecretValue",
                &json!({"SecretId":name}),
            )
            .await?;
            let value = result["SecretString"]
                .as_str()
                .ok_or(SecretError::Invalid(
                    "binary AWS secrets are not supported; use SecretString",
                ))?
                .to_owned();
            Ok::<_, SecretError>((name, value))
        })
        .buffer_unordered(8)
        .try_collect()
        .await
}

/// Creates or replaces one name; Firecrab replacements recheck value and version.
pub(crate) async fn upload(
    connection: &Connection,
    name: &str,
    value: &str,
    previous: Option<&str>,
) -> Result<(), SecretError> {
    let client = client()?;
    let exists = previous.is_some();
    match connection.summary.provider {
        Provider::Infisical => {
            let mut url = endpoint(connection, "api/v4/secrets/")?;
            url.path_segments_mut()
                .map_err(|_| SecretError::Provider)?
                .pop_if_empty()
                .push(name);
            let method = if exists { Method::PATCH } else { Method::POST };
            let settings = &connection.summary.settings;
            let body = json!({"projectId":settings.project_id,"environment":settings.environment,
                "secretPath":settings.secret_path,"secretValue":value,"type":"shared","skipMultilineEncoding":true});
            response_json(json_body(
                authorized(connection, client.request(method, url)),
                &body,
            ))
            .await?;
        }
        Provider::Firecrab => {
            let summaries = firecrab_summaries(&client, connection).await?;
            let existing = summaries
                .iter()
                .find(|item| item["name"].as_str() == Some(name));
            if existing.is_some() != exists {
                return Err(SecretError::Conflict);
            }
            let (method, path) = if let Some(item) = existing {
                let id = uuid::Uuid::parse_str(item["id"].as_str().ok_or(SecretError::Provider)?)
                    .map_err(|_| SecretError::Provider)?;
                let revealed = response_json(authorized(
                    connection,
                    client.post(endpoint(connection, &format!("api/secrets/{id}/reveal"))?),
                ))
                .await?;
                if revealed["value"].as_str() != previous || revealed["version"] != item["version"]
                {
                    return Err(SecretError::Conflict);
                }
                (Method::PUT, format!("api/secrets/{id}"))
            } else {
                (Method::POST, "api/secrets".into())
            };
            let mut body = json!({"name":name,"value":value});
            if let Some(item) = existing {
                body["description"] = item["description"].clone();
                body["version"] = item["version"].clone();
            }
            response_json(json_body(
                authorized(
                    connection,
                    client.request(method, endpoint(connection, &path)?),
                ),
                &body,
            ))
            .await?;
        }
        Provider::AwsSecretsManager => {
            if exists {
                aws_request(&client, connection, "PutSecretValue", &json!({"SecretId":name,"SecretString":value,"ClientRequestToken":uuid::Uuid::new_v4()})).await?;
            } else {
                aws_request(&client, connection, "CreateSecret", &json!({"Name":name,"SecretString":value,"ClientRequestToken":uuid::Uuid::new_v4()})).await?;
            }
        }
    }
    Ok(())
}

/// Sends a signed regional AWS JSON request, including a session token when supplied.
async fn aws_request(
    client: &Client,
    connection: &Connection,
    action: &str,
    body: &Value,
) -> Result<Value, SecretError> {
    let region = &connection.summary.settings.region;
    let domain = if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    let host = format!("secretsmanager.{region}.{domain}");
    let payload = body.to_string();
    let date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut headers = BTreeMap::from([
        ("content-type", "application/x-amz-json-1.1".to_owned()),
        ("host", host.clone()),
        ("x-amz-date", date.clone()),
        ("x-amz-target", format!("secretsmanager.{action}")),
    ]);
    if let Some(token) = connection
        .credentials
        .get("sessionToken")
        .filter(|value| !value.is_empty())
    {
        headers.insert("x-amz-security-token", token.clone());
    }
    let access_key = connection
        .credentials
        .get("accessKeyId")
        .ok_or(SecretError::Provider)?;
    let secret_key = connection
        .credentials
        .get("secretAccessKey")
        .ok_or(SecretError::Provider)?;
    let authorization = sign_aws(region, access_key, secret_key, &date, &headers, &payload);
    let mut request = client
        .post(format!("https://{host}/"))
        .header("authorization", authorization)
        .body(payload);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    response_json(request).await
}

/// Builds a SigV4 authorization header from sorted headers and the exact JSON payload.
fn sign_aws(
    region: &str,
    access_key: &str,
    secret_key: &str,
    date: &str,
    headers: &BTreeMap<&str, String>,
    payload: &str,
) -> String {
    let signed_headers = headers.keys().copied().collect::<Vec<_>>().join(";");
    let canonical_headers = headers
        .iter()
        .map(|(key, value)| {
            format!(
                "{key}:{}\n",
                value.split_whitespace().collect::<Vec<_>>().join(" ")
            )
        })
        .collect::<String>();
    let canonical = format!(
        "POST\n/\n\n{canonical_headers}\n{signed_headers}\n{:x}",
        Sha256::digest(payload)
    );
    let scope = format!("{}/{region}/secretsmanager/aws4_request", &date[..8]);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{:x}",
        Sha256::digest(canonical)
    );
    let mut key = format!("AWS4{secret_key}").into_bytes();
    for part in [&date[..8], region, "secretsmanager", "aws4_request"] {
        key = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &key), part.as_bytes())
            .as_ref()
            .to_vec();
    }
    let signature = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &key), to_sign.as_bytes());
    let signature: String = signature
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    )
}

#[cfg(test)]
mod tests;

/// Fetch only the linked account's values. Production callers supply the fixed console endpoint.
pub(crate) async fn account_snapshot(
    endpoint: &str,
    token: &str,
    account_id: uuid::Uuid,
) -> Result<BTreeMap<String, String>, SecretError> {
    let client = client()?;
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| SecretError::Provider)?;
    if matches!(response.status().as_u16(), 401 | 403 | 410) {
        return Err(SecretError::Invalid(
            "The account token expired, was revoked, or lacks secrets-read permission. Issue a secrets-read token in console.firecrab.dev and reconnect Auth",
        ));
    }
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err(SecretError::Provider);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.try_next().await.map_err(|_| SecretError::Provider)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err(SecretError::Provider);
        }
        bytes.extend_from_slice(&chunk);
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Snapshot {
        account_id: uuid::Uuid,
        secrets: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
        name: String,
        value: String,
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| SecretError::Provider)?;
    if snapshot.account_id != account_id || snapshot.secrets.len() > MAX_SECRETS {
        return Err(SecretError::Provider);
    }
    let mut values = BTreeMap::new();
    for entry in snapshot.secrets {
        validate_name(&entry.name)?;
        validate_value(&entry.value)?;
        if values.insert(entry.name, entry.value).is_some() {
            return Err(SecretError::Provider);
        }
    }
    Ok(values)
}
