use super::*;
use axum::{body::Bytes, http::HeaderMap, response::IntoResponse};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use std::sync::{Arc, atomic::AtomicBool};
use tokio::io::AsyncReadExt;
fn context(f: &Fixture) -> Context {
    let snapshot = f
        .state
        .store
        .code_verification_snapshot(f.id)
        .unwrap()
        .unwrap();
    let profile = code_agent_auth::resolve(
        snapshot.workspace.spec.agent.as_ref().unwrap(),
        snapshot.ai.as_ref().unwrap(),
    )
    .unwrap();
    Context {
        snapshot,
        record: f.record.clone(),
        profile,
        request: body(f),
    }
}
fn request(path: &str) -> broker::Request {
    broker::Request {
        kind: "request".into(),
        id: 7,
        method: "POST".into(),
        path: path.into(),
        headers: Default::default(),
        body: json!({"model":"fixture-model","stream":true,"input":"synthetic task"}),
    }
}
async fn server(status: StatusCode, stream: bool) -> (String, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let app = Router::new().fallback(post(move |headers: HeaderMap, Json(body): Json<Value>| {
        let calls = calls.clone();
        async move {
            assert!(
                headers
                    .get("authorization")
                    .is_some_and(|v| v == "Bearer host-only-synthetic-key")
                    || headers
                        .get("x-api-key")
                        .is_some_and(|v| v == "host-only-synthetic-key")
            );
            assert_eq!(body["model"], "fixture-model");
            calls.fetch_add(1, Ordering::SeqCst);
            if stream {
                let chunks = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(Bytes::from_static(b"data: {\"fixture\":true}\n\n"))
                })
                .chain(futures_util::stream::pending());
                return (
                    [("content-type", "text/event-stream")],
                    Body::from_stream(chunks),
                )
                    .into_response();
            }
            (
                status,
                [
                    ("content-type", "text/event-stream"),
                    ("location", "https://unassigned.invalid"),
                ],
                if status.is_success() {
                    "data: {\"fixture\":true}\n\n"
                } else {
                    "host-only-synthetic-key is in this upstream error"
                },
            )
                .into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), count)
}
#[tokio::test]
async fn code_run_broker_uses_only_assigned_host_key_and_frames_the_stream() {
    let mut f = fixture().await;
    let (origin, calls) = server(StatusCode::OK, false).await;
    f.state.code_agent_test_origin = Some(origin);
    let mut r = start(&f, Uuid::new_v4(), body(&f), driver()).await.unwrap();
    let c = context(&f);
    let (mut writer, mut reader) = tokio::io::duplex(65536);
    let mut req = request("/v1/responses");
    req.headers.insert(
        "authorization".into(),
        "Bearer unassigned-caller-key".into(),
    );
    broker::relay(&f.state, &c, &mut r, req, &mut writer)
        .await
        .unwrap();
    drop(writer);
    let mut frames = Vec::new();
    while let Ok(size) = reader.read_u32().await {
        let mut bytes = vec![0; size as usize];
        reader.read_exact(&mut bytes).await.unwrap();
        frames.push(serde_json::from_slice::<Value>(&bytes).unwrap());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(r.provider_requests, 1);
    assert_eq!(r.phase, Phase::Running);
    assert_eq!(frames.first().unwrap()["kind"], "start");
    assert_eq!(frames.last().unwrap()["kind"], "end");
    let data = frames
        .iter()
        .filter(|f| f["kind"] == "data")
        .flat_map(|f| STANDARD.decode(f["data"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(data, b"data: {\"fixture\":true}\n\n");
    assert!(
        !serde_json::to_string(&frames)
            .unwrap()
            .contains("host-only-synthetic-key")
    );
}
#[tokio::test]
async fn code_run_broker_blocks_wrong_model_origin_path_redirect_and_secret_errors() {
    let mut f = fixture().await;
    let (origin, calls) = server(StatusCode::OK, false).await;
    f.state.code_agent_test_origin = Some(origin);
    let mut r = start(&f, Uuid::new_v4(), body(&f), driver()).await.unwrap();
    let c = context(&f);
    let mut sink = tokio::io::sink();
    for path in [
        "https://unassigned.invalid/v1/responses",
        "/v1/files",
        "/v1/messages",
    ] {
        assert_eq!(
            broker::relay(&f.state, &c, &mut r, request(path), &mut sink).await,
            Err("provider_request_blocked")
        );
    }
    let mut wrong = request("/v1/responses");
    wrong.body["model"] = json!("other-model");
    assert_eq!(
        broker::relay(&f.state, &c, &mut r, wrong, &mut sink).await,
        Err("provider_request_blocked")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for (status, reason) in [
        (StatusCode::FOUND, "provider_unavailable"),
        (StatusCode::UNAUTHORIZED, "authentication_rejected"),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
    ] {
        let (origin, calls) = server(status, false).await;
        f.state.code_agent_test_origin = Some(origin);
        let (mut writer, mut reader) = tokio::io::duplex(65536);
        assert_eq!(
            broker::relay(&f.state, &c, &mut r, request("/v1/responses"), &mut writer).await,
            Err(reason)
        );
        drop(writer);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    assert_eq!(r.provider_requests, 0);
}
struct Streaming(Arc<AtomicBool>);
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl Driver for Streaming {
    async fn execute(
        &self,
        state: &AppState,
        c: &Context,
        r: &mut CodeAgentRun,
    ) -> Result<Outcome, &'static str> {
        let _guard = Dropped(self.0.clone());
        broker::relay(
            state,
            c,
            r,
            request("/v1/responses"),
            &mut tokio::io::sink(),
        )
        .await?;
        Ok(Outcome {
            code: 0,
            failed: false,
        })
    }
}
#[tokio::test]
async fn code_run_cancel_and_disconnect_abort_an_open_stream_and_release_workspace() {
    for disconnect in [false, true] {
        let mut f = fixture().await;
        let (origin, calls) = server(StatusCode::OK, true).await;
        f.state.code_agent_test_origin = Some(origin);
        let dropped = Arc::new(AtomicBool::new(false));
        let d = Box::leak(Box::new(Streaming(dropped.clone())));
        let id = Uuid::new_v4();
        start(&f, id, body(&f), d).await.unwrap();
        f.state.code_jobs.add_permits(1);
        phase(&f, id, Phase::Running).await;
        if disconnect {
            let c = context(&f);
            f.state
                .store
                .delete_ai_connection(c.snapshot.ai.unwrap().connection.id)
                .unwrap();
        } else {
            http(&f, "POST", &format!("/{id}/cancel")).await;
        }
        phase(
            &f,
            id,
            if disconnect {
                Phase::Interrupted
            } else {
                Phase::Cancelled
            },
        )
        .await;
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(crate::code_runtime::lock(&f.state, f.id).is_ok());
    }
}
