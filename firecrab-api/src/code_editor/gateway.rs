use super::*;
use axum::{
    body::{Body, to_bytes},
    extract::{
        FromRequestParts, Request,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

fn domain() -> String {
    std::env::var("FIRECRAB_CODE_EDITOR_DOMAIN")
        .unwrap_or_else(|_| "code.localhost".into())
        .to_ascii_lowercase()
}
fn authority(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST)?.to_str().ok()
}
fn hostname(host: &str) -> Option<&str> {
    if host.contains(['/', '@', '\\', '[', ']']) {
        return None;
    }
    host.split(':').next()
}
fn editor_host(host: &str) -> bool {
    hostname(host).is_some_and(|h| h.ends_with(&format!(".{}", domain())))
}
fn workspace(host: &str) -> Option<Uuid> {
    Uuid::parse_str(
        hostname(host)?
            .strip_suffix(&format!(".{}", domain()))?
            .split_once('-')?
            .0,
    )
    .ok()
}
pub(super) fn target(headers: &HeaderMap, key: Uuid, site: &str) -> Option<String> {
    let host = authority(headers)?;
    let name = hostname(host)?;
    let domain = domain();
    if !domain
        .split('.')
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    {
        return None;
    }
    if domain == "code.localhost" {
        if !matches!(name, "localhost" | "127.0.0.1") {
            return None;
        }
        let port = host
            .split_once(':')
            .map(|(_, p)| p.parse::<u16>())
            .transpose()
            .ok()?;
        Some(format!(
            "http://{}-{}.{}{}",
            key.simple(),
            site,
            domain,
            port.map(|p| format!(":{p}")).unwrap_or_default()
        ))
    } else {
        Some(format!("https://{}-{}.{}", key.simple(), site, domain))
    }
}
fn denied() -> Response {
    (
        StatusCode::FORBIDDEN,
        "CODE session is unavailable. Return to the workspace and open it again.",
    )
        .into_response()
}
fn safe(mut response: Response) -> Response {
    for (key, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("cross-origin-opener-policy", "same-origin"),
        ("x-frame-options", "SAMEORIGIN"),
    ] {
        response.headers_mut().insert(
            header::HeaderName::from_static(key),
            HeaderValue::from_static(value),
        );
    }
    response
}
/// Editor sites never inherit the controller's operator identity, REST routes or static dashboard.
pub(crate) async fn isolate(
    State((state, root)): State<(AppState, Option<PathBuf>)>,
    request: Request,
    next: Next,
) -> Response {
    let host = authority(request.headers()).unwrap_or("").to_owned();
    if !editor_host(&host) {
        if request
            .headers()
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok())
            .and_then(|o| reqwest::Url::parse(o).ok())
            .and_then(|u| u.host_str().map(str::to_owned))
            .is_some_and(|h| editor_host(&h))
        {
            return safe(denied());
        }
        let mut response = next.run(request).await;
        response.headers_mut().insert(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("SAMEORIGIN"),
        );
        return response;
    }
    safe(serve(state, root, host, request).await)
}
async fn serve(state: AppState, root: Option<PathBuf>, host: String, request: Request) -> Response {
    let Some(key) = workspace(&host) else {
        return denied();
    };
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !origin
            .to_str()
            .ok()
            .and_then(|v| reqwest::Url::parse(v).ok())
            .is_some_and(|u| matches!(u.scheme(), "http" | "https") && u.authority() == host)
    {
        return denied();
    }
    if let Some(ticket) = request.uri().path().strip_prefix("/_fc/connect/") {
        if request.method() != axum::http::Method::GET {
            return denied();
        }
        let ticket = state
            .code_editor
            .tickets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(ticket);
        let Some(t) = ticket.filter(|t| {
            t.expires > Instant::now()
                && t.session.record.runtime.workspace_id == key
                && hostname(&host).is_some_and(|h| {
                    h == format!("{}-{}.{}", key.simple(), t.session.site, domain())
                })
                && current(&state, &t.session)
        }) else {
            return denied();
        };
        let mut response = Redirect::to(if t.terminal {
            "/_fc/terminal"
        } else {
            "/?folder=/home/fc-code/project"
        })
        .into_response();
        let secure = if domain() == "code.localhost" {
            ""
        } else {
            "; Secure"
        };
        let value = format!(
            "fc_code_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=3600{secure}",
            t.session.cookie
        );
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&value).expect("generated cookie"),
        );
        return response;
    }
    let Some(s) = find(&state, key) else {
        return denied();
    };
    if !hostname(&host).is_some_and(|h| h == format!("{}-{}.{}", key.simple(), s.site, domain())) {
        return denied();
    }
    if *s.phase.lock().unwrap_or_else(|p| p.into_inner()) != "ready" {
        return denied();
    }
    let authorized = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|c| c.trim().strip_prefix("fc_code_session="))
        .any(|v| {
            bool::from(subtle::ConstantTimeEq::ct_eq(
                v.as_bytes(),
                s.cookie.as_bytes(),
            ))
        });
    if !authorized {
        return denied();
    }
    let Ok(permit) = s.slots.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let path = request.uri().path();
    if path.starts_with("/api/") || path.starts_with("/ws/") || path == "/api" || path == "/ws" {
        return denied();
    }
    if path == "/_fc/terminal" {
        if let Some(root) = root {
            use tower::ServiceExt;
            return tower_http::services::ServeFile::new(root.join("code-terminal.html"))
                .oneshot(request)
                .await
                .unwrap()
                .map(Body::new);
        }
        return axum::response::Html(include_str!(
            "../../../firecrab-frontend/code-terminal.html"
        ))
        .into_response();
    }
    if path == "/_fc/terminal/socket" {
        return super::terminal::upgrade(state, s, request, permit).await;
    }
    if path.starts_with("/_fc/") {
        return denied();
    }
    if path.starts_with("/assets/") {
        if let Some(root) = root {
            use tower::ServiceExt;
            return tower_http::services::ServeDir::new(root)
                .oneshot(request)
                .await
                .unwrap()
                .map(Body::new);
        }
        return denied();
    }
    if request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
    {
        return websocket(s, host, request, permit).await;
    }
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, 16 * 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let url = format!(
        "http://{host}{}",
        parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
    );
    let mut upstream = s.client.request(parts.method, &url).body(body);
    for (name, value) in &parts.headers {
        if allowed(name.as_str()) {
            upstream = upstream.header(name, value);
        }
    }
    upstream = upstream.header("x-forwarded-host", &host).header(
        "x-forwarded-proto",
        if domain() == "code.localhost" {
            "http"
        } else {
            "https"
        },
    );
    let Ok(response) = upstream.send().await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let mut builder = Response::builder().status(response.status());
    for (name, value) in response.headers() {
        if allowed(name.as_str())
            && name != header::SET_COOKIE
            && name != header::ACCESS_CONTROL_ALLOW_ORIGIN
            && name.as_str() != "clear-site-data"
        {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(Body::from_stream(response.bytes_stream().map(
            move |value| {
                let _ = &permit;
                value
            },
        )))
        .unwrap()
}
fn allowed(name: &str) -> bool {
    !matches!(
        name,
        "authorization"
            | "cookie"
            | "host"
            | "connection"
            | "upgrade"
            | "keep-alive"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "x-firecrab-controller-id"
    ) && !name.starts_with("x-forwarded-")
}
async fn websocket(
    s: Arc<Session>,
    host: String,
    request: Request,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Response {
    let (mut parts, _) = request.into_parts();
    let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
        return denied();
    };
    let url = format!(
        "ws://{host}{}",
        parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
    );
    let Ok(mut upstream) = url.into_client_request() else {
        return denied();
    };
    for name in [header::ORIGIN, header::SEC_WEBSOCKET_PROTOCOL] {
        if let Some(v) = parts.headers.get(&name) {
            upstream.headers_mut().insert(name, v.clone());
        }
    }
    upstream.headers_mut().insert(
        "x-forwarded-host",
        HeaderValue::from_str(&host).expect("validated authority"),
    );
    upstream.headers_mut().insert(
        "x-forwarded-proto",
        HeaderValue::from_static(if domain() == "code.localhost" {
            "http"
        } else {
            "https"
        }),
    );
    let Ok(stream) = UnixStream::connect(&s.socket).await else {
        return denied();
    };
    let Ok((remote, response)) = tokio_tungstenite::client_async(upstream, stream).await else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let protocols: Vec<String> = response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(|s| vec![s.to_owned()])
        .unwrap_or_default();
    upgrade.protocols(protocols).max_message_size(8*1024*1024).on_upgrade(move|mut browser|async move{
        let _permit=permit;
        let mut remote=remote;let mut stop=s.cancel.subscribe();
        let work=async{loop {tokio::select!{
            message=browser.recv()=>{let Some(Ok(message))=message else{break;};let converted=match message{Message::Text(v)=>tungstenite::Message::Text(v.to_string().into()),Message::Binary(v)=>tungstenite::Message::Binary(v),Message::Ping(v)=>tungstenite::Message::Ping(v),Message::Pong(v)=>tungstenite::Message::Pong(v),Message::Close(_)=>break};if remote.send(converted).await.is_err(){break;}},
            message=remote.next()=>{let Some(Ok(message))=message else{break;};let converted=match message{tungstenite::Message::Text(v)=>Message::Text(v.to_string().into()),tungstenite::Message::Binary(v)=>Message::Binary(v),tungstenite::Message::Ping(v)=>Message::Ping(v),tungstenite::Message::Pong(v)=>Message::Pong(v),tungstenite::Message::Close(_)=>break,_=>continue};if browser.send(converted).await.is_err(){break;}}
        }}};tokio::select!{_=work=>{},_=stop.changed()=>{}}
        let _=tokio::time::timeout(Duration::from_millis(100),async{let _=browser.close().await;let _=remote.close(None).await;}).await;
    })
}
