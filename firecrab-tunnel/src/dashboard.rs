//! Same-origin token issuance and revocation for the public dashboard.
use super::*;
use axum::http::header;
use firecrab_helper_protocol::firecrab_tunnel::valid_token;

/// Keep issued credentials and dashboard pages out of caches and referrers.
fn protected(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("content-security-policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    response
}

/// Read a bounded bearer secret without including it in any error message.
fn bearer(request: &Request<Body>) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|value| valid_token(value))
}

/// Check dashboard ownership while the caller holds the registry transaction lock.
pub(super) fn authorize(
    db: &Connection,
    digest: &[u8],
    name: &str,
    deleting: bool,
) -> Result<bool, StatusCode> {
    let issued: Option<(String, bool)> = db
        .query_row(
            "SELECT name,revoked FROM connection_tokens WHERE owner=?1",
            [digest],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if let Some((issued_name, revoked)) = issued {
        if revoked {
            return Err(StatusCode::GONE);
        }
        if issued_name != name {
            return Err(StatusCode::FORBIDDEN);
        }
        return Ok(true);
    }
    let reserved: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_tokens WHERE name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if reserved {
        return Err(StatusCode::CONFLICT);
    }
    // Legacy ownership can update an existing reservation, but cannot mint a new one.
    let existing: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM addresses WHERE name=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if !existing && !deleting {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(false)
}

impl Service {
    /// Resolve an issued token to its server-assigned public address.
    pub(super) fn token_info(&self, request: &Request<Body>) -> Response {
        let Some(token) = bearer(request) else {
            return protected(error(StatusCode::UNAUTHORIZED, "connection_token_required"));
        };
        let digest = Sha256::digest(token.as_bytes()).to_vec();
        let db = self.db.lock().unwrap();
        let issued: rusqlite::Result<Option<(String, bool)>> = db
            .query_row(
                "SELECT name,revoked FROM connection_tokens WHERE owner=?1",
                [&digest],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional();
        protected(match issued {
            Ok(Some((_, true))) => error(StatusCode::GONE, "connection_token_revoked"),
            Ok(Some((name, false))) => axum::Json(serde_json::json!({
                "subdomain": name, "publicUrl": format!("https://{name}.{}", self.domain)
            }))
            .into_response(),
            Ok(None) => error(StatusCode::UNAUTHORIZED, "connection_token_invalid"),
            Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable"),
        })
    }

    /// Serve dashboard assets and require same-origin JSON for credential mutations.
    pub(super) async fn dashboard(&self, request: Request<Body>) -> Response {
        let path = request.uri().path();
        if request.method() == "GET" {
            let asset = match path {
                "/dashboard" | "/dashboard/" => {
                    Some(("text/html; charset=utf-8", include_str!("dashboard.html")))
                }
                "/dashboard/app.js" => Some((
                    "text/javascript; charset=utf-8",
                    include_str!("dashboard.js"),
                )),
                "/dashboard/style.css" => {
                    Some(("text/css; charset=utf-8", include_str!("dashboard.css")))
                }
                _ => None,
            };
            return protected(match asset {
                Some((kind, content)) => ([(header::CONTENT_TYPE, kind)], content).into_response(),
                None => error(StatusCode::NOT_FOUND, "not_found"),
            });
        }
        if request.method() != "POST" || !matches!(path, "/dashboard/tokens" | "/dashboard/revoke")
        {
            return protected(error(StatusCode::NOT_FOUND, "not_found"));
        }
        let same_origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .and_then(|value| reqwest::Url::parse(value).ok())
            .is_some_and(|origin| {
                matches!(origin.scheme(), "http" | "https")
                    && origin.host_str() == Some(self.domain.as_str())
                    && origin.path() == "/"
                    && origin.query().is_none()
                    && origin.fragment().is_none()
                    && origin.username().is_empty()
                    && origin.password().is_none()
                    && request
                        .headers()
                        .get(header::HOST)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|authority| {
                            reqwest::Url::parse(&format!("{}://{authority}", origin.scheme())).ok()
                        })
                        .is_some_and(|expected| expected.origin() == origin.origin())
            });
        let json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|kind| kind.split(';').next().unwrap_or("").trim() == "application/json");
        if !same_origin || !json {
            return protected(error(StatusCode::FORBIDDEN, "same_origin_json_required"));
        }
        let revoke = path == "/dashboard/revoke";
        let peer = self.peer(&request);
        let Ok(body) = to_bytes(request.into_body(), 1024).await else {
            return protected(error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large"));
        };
        if revoke {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Revoke {
                token: String,
            }
            let Ok(value) = serde_json::from_slice::<Revoke>(&body) else {
                return protected(error(StatusCode::BAD_REQUEST, "invalid_request"));
            };
            if !valid_token(&value.token) {
                return protected(error(StatusCode::UNAUTHORIZED, "connection_token_invalid"));
            }
            return protected(self.revoke_token(&value.token));
        }
        if !serde_json::from_slice::<serde_json::Value>(&body)
            .is_ok_and(|value| value.as_object().is_some_and(|object| object.is_empty()))
        {
            return protected(error(StatusCode::BAD_REQUEST, "invalid_request"));
        }
        protected(self.issue_token(&peer))
    }

    /// Atomically reserve a generated hostname and retain only the token digest.
    fn issue_token(&self, peer: &str) -> Response {
        let mut db = self.db.lock().unwrap();
        let Ok(transaction) = db.transaction() else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable");
        };
        let count = transaction
            .query_row("SELECT count(*) FROM addresses", [], |row| row.get(0))
            .unwrap_or(i64::MAX);
        if let Some(response) = self.registration_limit(peer, count) {
            return response;
        }
        let name = format!("app-{}", uuid::Uuid::new_v4().simple());
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let digest = Sha256::digest(token.as_bytes()).to_vec();
        if transaction
            .execute(
                "INSERT INTO connection_tokens(owner,name) VALUES(?1,?2)",
                params![digest, name],
            )
            .is_err()
            || transaction
                .execute(
                    "INSERT INTO addresses(name,owner,target,enabled,seen) VALUES(?1,?2,NULL,0,0)",
                    params![name, digest],
                )
                .is_err()
            || transaction.commit().is_err()
        {
            return error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable");
        }
        (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({
                "token": token, "publicUrl": format!("https://{name}.{}", self.domain)
            })),
        )
            .into_response()
    }

    /// Retain a revocation tombstone so a stopped updater cannot revive the credential.
    fn revoke_token(&self, token: &str) -> Response {
        let digest = Sha256::digest(token.as_bytes()).to_vec();
        let mut db = self.db.lock().unwrap();
        let Ok(transaction) = db.transaction() else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable");
        };
        let changed = match transaction.execute(
            "UPDATE connection_tokens SET revoked=1 WHERE owner=?1",
            [&digest],
        ) {
            Ok(changed) => changed,
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable"),
        };
        if changed == 0 {
            return error(StatusCode::UNAUTHORIZED, "connection_token_invalid");
        }
        if transaction
            .execute(
                "UPDATE addresses SET target=NULL,enabled=0,seen=0 WHERE owner=?1",
                [&digest],
            )
            .is_err()
            || transaction.commit().is_err()
        {
            return error(StatusCode::SERVICE_UNAVAILABLE, "registry_unavailable");
        }
        StatusCode::NO_CONTENT.into_response()
    }
}
