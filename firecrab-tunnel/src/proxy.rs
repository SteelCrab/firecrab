//! Streaming proxy. Public Host is forwarded explicitly to the selected Nginx route group.
use axum::{
    body::Body,
    extract::{
        FromRequestParts,
        ws::{CloseFrame, Message, WebSocketUpgrade},
    },
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Message as UpstreamMessage, client::IntoClientRequest};

/// Remove hop-by-hop headers and headers nominated by Connection.
fn clean(headers: &mut HeaderMap) {
    let nominated = headers
        .get("connection")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(|v| v.trim().to_owned())
        .collect::<Vec<_>>();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "host",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

/// Stream HTTP or WebSocket traffic while preserving the public forwarding host.
pub(super) async fn forward(
    client: reqwest::Client,
    request: Request<Body>,
    origin: &str,
    host: &str,
) -> Response {
    let mut url = reqwest::Url::parse(origin).unwrap();
    url.set_path(request.uri().path());
    url.set_query(request.uri().query());
    let (mut parts, body) = request.into_parts();
    let upgrade = parts
        .headers
        .get("upgrade")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if upgrade {
        let Ok(socket) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        if url.set_scheme(scheme).is_err() {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        let Ok(mut outgoing) = url.as_str().into_client_request() else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        clean(&mut parts.headers);
        for name in [
            "sec-websocket-key",
            "sec-websocket-version",
            "sec-websocket-extensions",
        ] {
            parts.headers.remove(name);
        }
        outgoing.headers_mut().extend(parts.headers);
        outgoing
            .headers_mut()
            .insert("x-forwarded-host", host.parse().unwrap());
        outgoing
            .headers_mut()
            .insert("x-forwarded-proto", "https".parse().unwrap());
        let Ok(Ok((upstream, response))) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(outgoing),
        )
        .await
        else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        let socket = if let Some(protocol) = response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
        {
            socket.protocols([protocol.to_owned()])
        } else {
            socket
        };
        return socket.on_upgrade(move |socket| async move {
            let (mut client_send, mut client_read) = socket.split();
            let (mut upstream_send, mut upstream_read) = upstream.split();
            tokio::select! {
                _ = async {
                    while let Some(Ok(message)) = client_read.next().await {
                        let message = match message {
                            Message::Text(v) => UpstreamMessage::Text(v.to_string().into()),
                            Message::Binary(v) => UpstreamMessage::Binary(v),
                            Message::Ping(v) => UpstreamMessage::Ping(v),
                            Message::Pong(v) => UpstreamMessage::Pong(v),
                            Message::Close(frame) => UpstreamMessage::Close(frame.map(|frame| {
                                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                    code: frame.code.into(),
                                    reason: frame.reason.to_string().into(),
                                }
                            })),
                        };
                        if upstream_send.send(message).await.is_err() {
                            break;
                        }
                    }
                } => {},
                _ = async {
                    while let Some(Ok(message)) = upstream_read.next().await {
                        let message = match message {
                            UpstreamMessage::Text(v) => Message::Text(v.to_string().into()),
                            UpstreamMessage::Binary(v) => Message::Binary(v),
                            UpstreamMessage::Ping(v) => Message::Ping(v),
                            UpstreamMessage::Pong(v) => Message::Pong(v),
                            UpstreamMessage::Close(frame) => Message::Close(frame.map(|frame| {
                                CloseFrame {
                                    code: frame.code.into(),
                                    reason: frame.reason.to_string().into(),
                                }
                            })),
                            UpstreamMessage::Frame(_) => continue,
                        };
                        if client_send.send(message).await.is_err() {
                            break;
                        }
                    }
                } => {},
            }
        });
    }
    clean(&mut parts.headers);
    parts
        .headers
        .insert("x-forwarded-host", host.parse().unwrap());
    parts
        .headers
        .insert("x-forwarded-proto", "https".parse().unwrap());
    let outgoing = client
        .request(parts.method, url)
        .headers(parts.headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()));
    let response =
        match tokio::time::timeout(std::time::Duration::from_secs(30), outgoing.send()).await {
            Ok(Ok(v)) => v,
            _ => return StatusCode::BAD_GATEWAY.into_response(),
        };
    let status = response.status();
    let mut headers = response.headers().clone();
    clean(&mut headers);
    if let Some(location) = headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix(origin))
        .map(str::to_owned)
        && (location.is_empty() || location.starts_with('/') || location.starts_with('?'))
    {
        headers.insert(
            "location",
            format!("https://{host}{location}").parse().unwrap(),
        );
    }
    let mut result = Response::new(Body::from_stream(response.bytes_stream()));
    *result.status_mut() = status;
    *result.headers_mut() = headers;
    result
}
