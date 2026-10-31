//! A reserved editor origin can never fall through to controller routes.
use super::*;
use axum::{
    body::Body,
    extract::{FromRequestParts, Request},
    http::{HeaderValue, StatusCode, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use tower::ServiceExt;

const SUFFIX: &str = ".editor.localhost";
/// Encode UUID bytes into a lowercase DNS-safe label without losing identity.
fn encode(id: Uuid) -> String {
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut output = String::new();
    let mut bits = 0u32;
    let mut n = 0;
    for b in id.as_bytes() {
        bits = (bits << 8) | u32::from(*b);
        n += 8;
        while n >= 5 {
            n -= 5;
            output.push(char::from(alphabet[((bits >> n) & 31) as usize]));
        }
    }
    if n > 0 {
        output.push(char::from(alphabet[((bits << (5 - n)) & 31) as usize]));
    }
    output
}
/// Require the canonical unpadded encoding of a non-empty UUID.
fn decode(text: &str) -> Result<Uuid, Error> {
    if text.len() != 26 {
        return Err(Error::Invalid("Invalid editor host identity"));
    }
    let mut bytes = Vec::new();
    let mut bits = 0u32;
    let mut n = 0;
    for b in text.bytes() {
        let value = match b {
            b'a'..=b'z' => b - b'a',
            b'2'..=b'7' => b - b'2' + 26,
            _ => return Err(Error::Invalid("Invalid editor host identity")),
        };
        bits = (bits << 5) | u32::from(value);
        n += 5;
        if n >= 8 {
            n -= 8;
            bytes.push((bits >> n) as u8);
        }
    }
    let id =
        Uuid::from_slice(&bytes).map_err(|_| Error::Invalid("Invalid editor host identity"))?;
    if id.is_nil() || encode(id) != text {
        return Err(Error::Invalid("Invalid editor host identity"));
    }
    Ok(id)
}
/// Put project and private editor identities into one bounded DNS label.
pub(super) fn host(project: Uuid, editor: Uuid) -> String {
    format!("{}-{}{SUFFIX}", encode(project), encode(editor))
}
/// Reserve the entire editor hostname suffix, including malformed scoped hosts.
fn classify(authority: &str) -> Option<Result<(Uuid, Uuid), Error>> {
    let authority = authority.to_ascii_lowercase();
    let name = authority
        .split(':')
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    if name == "editor.localhost" {
        return Some(Err(Error::Invalid("Use a scoped editor host")));
    }
    let label = name.strip_suffix(SUFFIX)?;
    Some(
        label
            .split_once('-')
            .ok_or(Error::Invalid("Invalid editor host identity"))
            .and_then(|(p, e)| Ok((decode(p)?, decode(e)?))),
    )
}
/// Keep editor-origin requests out of every controller route.
pub(crate) async fn dispatch(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0)
        .unwrap_or_else(Uuid::new_v4);
    let authority = request.uri().authority().map(|a| a.as_str());
    let hosts: Vec<_> = request
        .headers()
        .get_all(header::HOST)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    let editor_authority = authority
        .and_then(|a| classify(a).map(|r| (a, r)))
        .or_else(|| hosts.iter().find_map(|h| classify(h).map(|r| (*h, r))));
    if editor_authority.is_some()
        && (hosts.len() > 1
            || authority.is_some_and(|a| hosts.iter().any(|h| !a.eq_ignore_ascii_case(h))))
    {
        return Error::Invalid("Conflicting editor host authorities")
            .response(id)
            .into_response();
    }
    let Some((_, route)) = editor_authority else {
        // An editor is not a trusted cross-origin dashboard, even if separately allowlisted.
        if request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| reqwest::Url::parse(s).ok())
            .and_then(|u| u.host_str().map(String::from))
            .is_some_and(|h| classify(&h).is_some())
        {
            return denied(id).into_response();
        }
        return next.run(request).await;
    };
    match route {
        Err(e) => e.response(id).into_response(),
        Ok((project, editor)) => match serve(state, request, project, editor, id).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        },
    }
}
/// Supply the actor-owned workbench token without URL credentials or parent-domain cookies.
fn workbench_configuration(bytes: &[u8], token: &str) -> Result<Vec<u8>, Error> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Conflict("Invalid retained editor authentication"));
    }
    let mut html = std::str::from_utf8(bytes)
        .map_err(|_| Error::Conflict("Invalid editor workbench document"))?
        .to_string();
    let identity = "id=\"vscode-workbench-web-configuration\"";
    let mut matches = html.match_indices(identity);
    let start = matches
        .next()
        .map(|(at, _)| at)
        .ok_or(Error::Conflict("Editor workbench configuration is missing"))?;
    if matches.next().is_some() {
        return Err(Error::Conflict(
            "Editor workbench configuration is ambiguous",
        ));
    }
    let end = html[start..]
        .find('>')
        .ok_or(Error::Conflict("Invalid editor workbench configuration"))?
        + start;
    let marker = "data-settings=\"{";
    let offset = html[start..end].find(marker).ok_or(Error::Conflict(
        "Unsupported editor workbench configuration",
    ))? + start
        + marker.len();
    html.insert_str(
        offset,
        &format!("&quot;connectionToken&quot;:&quot;{token}&quot;,"),
    );
    Ok(html.into_bytes())
}
/// Authorize private workbench traffic and recheck active streams after upgrades.
async fn serve(
    state: AppState,
    request: Request,
    project: Uuid,
    editor: Uuid,
    id: Uuid,
) -> Result<Response, AppError> {
    let permit = std::sync::Arc::new(
        state
            .workspaces
            .editor_connections
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy.response(id))?,
    );
    let (mut parts, body) = request.into_parts();
    let workbench = parts.method == axum::http::Method::GET && parts.uri.path() == "/";
    let caller = Caller::from_request_parts(&mut parts, &state).await?;
    let local = state.clone();
    let (b, record, directory) = blocking(id, move || {
        caller
            .project(&local, project, SandboxPermission::Read)
            .map_err(|_| Error::Denied)?;
        let b = binding(&local, project)?;
        let cached = local
            .workspaces
            .editor_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(project, editor))
            .cloned();
        let e = match cached {
            Some(e) => e,
            None => {
                let db = open(&local, &b)?;
                let e = db.editor(caller, editor, false)?;
                local
                    .workspaces
                    .editor_cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert((project, editor), e.clone());
                e
            }
        };
        let directory = super::super::db::project_directory(&local.workspaces.root, &b, false)?;
        current(&local, caller, &b, &e, id).map_err(|_| Error::Denied)?;
        if e.status != "ready" || e.proof.is_none() {
            return Err(Error::Conflict("The private editor is not ready"));
        }
        Ok((b, e, directory))
    })
    .await?;
    let path = parts.uri.path().to_string();
    if let Some(endpoint) = path.strip_prefix("/_firecrab/") {
        if endpoint == "session" {
            if parts.method != axum::http::Method::GET {
                return Err(Error::Invalid("Use GET for editor session state").response(id));
            }
            return Ok(Json(response(project, &record)).into_response());
        }
        let suffix = match endpoint {
            "files" => "files".to_string(),
            "tree" => "files/tree".into(),
            "draft" => format!("views/{}/draft", record.view),
            "drafts" => format!("views/{}/drafts", record.view),
            _ => return Err(Error::Missing.response(id)),
        };
        let query = parts
            .uri
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        parts.uri = format!("/api/ai-sandbox/projects/{project}/workspace/{suffix}{query}")
            .parse::<Uri>()
            .map_err(|_| Error::Invalid("Invalid editor file path").response(id))?;
        let payload = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            axum::body::to_bytes(body, 2 * 1024 * 1024),
        )
        .await
        .map_err(|_| Error::Invalid("Editor file request timed out").response(id))?
        .map_err(|_| Error::Invalid("Editor file request exceeds its limit").response(id))?;
        // A startup scan performs concurrent file reads. Wait for a definitive lock
        // rejection instead of permanently failing the filesystem extension's activation.
        for attempt in 0..12 {
            current(&state, caller, &b, &record, id)?;
            let mut replay = Request::new(Body::from(payload.clone()));
            *replay.method_mut() = parts.method.clone();
            *replay.uri_mut() = parts.uri.clone();
            *replay.headers_mut() = parts.headers.clone();
            *replay.extensions_mut() = parts.extensions.clone();
            let response = super::super::routes()
                .with_state(state.clone())
                .oneshot(replay)
                .await
                .map_err(|_| AppError::internal(id))?;
            if response.status() != StatusCode::CONFLICT {
                return Ok(response);
            }
            let (head, body) = response.into_parts();
            let bytes = axum::body::to_bytes(body, 16 * 1024)
                .await
                .map_err(|_| AppError::internal(id))?;
            let error: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            if error["error"]["code"] != "workspace_busy" || attempt == 11 {
                return Ok(Response::from_parts(head, Body::from(bytes)));
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                (100 * (attempt + 1)).min(500),
            ))
            .await;
        }
        unreachable!();
    }
    let proof = record
        .proof
        .as_ref()
        .ok_or_else(|| Error::Conflict("Missing editor process identity").response(id))?;
    let package = package::guest_package().map_err(|e| e.response(id))?;
    let socket = format!("/workspace/shared/.firecrab-workspace/editors/{editor}/data/server.sock");
    let channel = guest::project_editor_stream(
        &state,
        &record.runtime,
        &directory,
        &package,
        &socket,
        proof.uid,
    )
    .map_err(|_| Error::Conflict("Editor transport is unavailable").response(id))?;
    let (mut client, connection) =
        hyper::client::conn::http1::handshake::<_, Body>(hyper_util::rt::TokioIo::new(channel))
            .await
            .map_err(|_| Error::Conflict("Editor transport handshake failed").response(id))?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        axum::body::to_bytes(body, 2 * 1024 * 1024),
    )
    .await
    .map_err(|_| Error::Invalid("Editor request body timed out").response(id))?
    .map_err(|_| Error::Invalid("Editor request exceeds 2 MiB").response(id))?;
    let mut outgoing = Request::from_parts(parts, Body::from(body));
    let browser_upgrade = hyper::upgrade::on(&mut outgoing);
    outgoing.headers_mut().remove(header::AUTHORIZATION);
    outgoing.headers_mut().remove(header::COOKIE);
    outgoing.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_str(&format!("vscode-tkn={}", record.token))
            .map_err(|_| AppError::internal(id))?,
    );
    for name in [
        "forwarded",
        "x-forwarded-host",
        "x-forwarded-for",
        "x-firecrab-controller-id",
    ] {
        outgoing.headers_mut().remove(name);
    }
    let mut result = client.send_request(outgoing).await.map_err(|_| {
        Error::Conflict("Editor is disconnected; explicitly reopen if its runtime ended")
            .response(id)
    })?;
    current(&state, caller, &b, &record, id)?;
    if result.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut result);
        let state = state.clone();
        let b = b.clone();
        let record = record.clone();
        let retained_permit = permit.clone();
        tokio::spawn(async move {
            let _permit = retained_permit;
            let (Ok(browser), Ok(upstream)) = tokio::join!(browser_upgrade, upstream_upgrade)
            else {
                return;
            };
            let mut browser = hyper_util::rt::TokioIo::new(browser);
            let mut upstream = hyper_util::rt::TokioIo::new(upstream);
            let copy = tokio::io::copy_bidirectional(&mut browser, &mut upstream);
            tokio::pin!(copy);
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tokio::select! {_= &mut copy=>break,_=timer.tick()=>if current(&state,caller,&b,&record,id).is_err(){break;}}
            }
        });
    }
    let (status, mut headers) = (result.status(), result.headers().clone());
    let parent_origin = record.parent_origin.clone();
    let body = if workbench && status == StatusCode::OK {
        if headers
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(
                Error::Conflict("Unsupported compressed workbench configuration").response(id),
            );
        }
        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            axum::body::to_bytes(Body::new(result.into_body()), 2 * 1024 * 1024),
        )
        .await
        .map_err(|_| Error::Conflict("Workbench configuration timed out").response(id))?
        .map_err(|_| Error::Conflict("Workbench configuration exceeds its limit").response(id))?;
        let html = workbench_configuration(&bytes, &record.token).map_err(|e| e.response(id))?;
        current(&state, caller, &b, &record, id)?;
        headers.remove(header::CONTENT_LENGTH);
        headers.remove(header::ETAG);
        Body::from(html)
    } else {
        let body = result.into_body().into_data_stream().map(move |chunk| {
            let _permit = &permit;
            if current(&state, caller, &b, &record, id).is_err() {
                Err(std::io::Error::other("Editor access ended"))
            } else {
                chunk.map_err(std::io::Error::other)
            }
        });
        Body::from_stream(body)
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    response.headers_mut().remove(header::SET_COOKIE);
    response.headers_mut().remove("clear-site-data");
    response.headers_mut().remove(header::X_FRAME_OPTIONS);
    // Keep the workbench's other CSP directives; only authorize its explicit dashboard parent.
    if let Some(policy) = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
    {
        let mut directives: Vec<_> = policy
            .split(';')
            .map(str::trim)
            .filter(|s| !s.starts_with("frame-ancestors"))
            .map(String::from)
            .collect();
        directives.push(format!("frame-ancestors 'self' {parent_origin}"));
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&directives.join("; ")).map_err(|_| AppError::internal(id))?,
        );
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workbench_uses_only_its_own_configuration_token_and_rejects_ambiguous_documents() {
        let original = br#"<meta id="vscode-workbench-web-configuration" data-settings="{&quot;remoteAuthority&quot;:&quot;scoped.editor.localhost&quot;}">"#;
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let first = String::from_utf8(workbench_configuration(original, &a).unwrap()).unwrap();
        let second = String::from_utf8(workbench_configuration(original, &b).unwrap()).unwrap();
        assert!(first.contains(&a));
        assert!(!first.contains(&b));
        assert!(second.contains(&b));
        assert!(first.contains("&quot;remoteAuthority&quot;:&quot;scoped.editor.localhost&quot;"));
        assert!(
            workbench_configuration(&[original.as_slice(), original.as_slice()].concat(), &a)
                .is_err()
        );
        assert!(workbench_configuration(b"<html></html>", &a).is_err());
        assert!(workbench_configuration(original, "invalid").is_err());
    }
    #[test]
    fn hosts_round_trip_and_reject_unscoped_or_noncanonical_names() {
        for _ in 0..32 {
            let p = Uuid::new_v4();
            let e = Uuid::new_v4();
            let h = host(p, e);
            assert!(h.split('.').next().unwrap().len() <= 63);
            assert_eq!(classify(&h).unwrap().unwrap(), (p, e));
            assert_eq!(classify(&h.to_uppercase()).unwrap().unwrap(), (p, e));
        }
        assert!(classify("localhost:5523").is_none());
        assert!(classify("bad.editor.localhost").unwrap().is_err());
        assert!(classify("editor.localhost").unwrap().is_err());
        assert!(classify("x.editor.localhost.evil.test").is_none());
    }
}
