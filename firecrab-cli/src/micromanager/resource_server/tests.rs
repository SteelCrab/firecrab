use super::*;
#[test]
fn only_loopback_hosts_and_trusted_browser_origins_are_accepted() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:5524"));
    assert!(allowed(&headers));
    headers.insert("origin", HeaderValue::from_static("http://localhost:5523"));
    assert!(allowed(&headers));
    headers.insert("origin", HeaderValue::from_static("https://evil.example"));
    assert!(!allowed(&headers));
    headers.remove("origin");
    headers.insert("host", HeaderValue::from_static("evil.example:5524"));
    assert!(!allowed(&headers));
}
use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

fn request(method: &str, path: &str, id: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5524")
        .header("origin", ORIGINS[0])
        .header("content-type", "application/json");
    if let Some(id) = id {
        builder = builder.header("x-firecrab-controller-id", id);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

async fn response(app: Router, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let result = app.oneshot(request).await.unwrap();
    let status = result.status();
    let headers = result.headers().clone();
    let bytes = to_bytes(result.into_body(), 16384).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, body)
}

#[tokio::test]
async fn local_requests_read_save_and_validate_the_same_managed_home() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::tests::platform();
    let app = router_for(dir.path().to_owned(), platform);
    let path = "/api/service/resources";
    let (status, headers, current) = response(app.clone(), request("GET", path, None, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["access-control-allow-origin"], ORIGINS[0]);
    assert_eq!(headers["cache-control"], "no-store");
    let id = current["controllerId"].as_str().unwrap();
    let mut settings = current["settings"].clone();
    settings["cpu"] = json!(6);
    let (status, _, saved) = response(
        app.clone(),
        request("PUT", path, Some(id), &settings.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["settings"], settings);
    assert_eq!(saved["pending"], true);
    let bytes = fs::read(dir.path().join("resources.json")).unwrap();
    settings["cpu"] = json!(0);
    let (status, _, error) = response(
        app.clone(),
        request("PUT", path, Some(id), &settings.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"].as_str().unwrap().contains("CPU"));
    assert_eq!(fs::read(dir.path().join("resources.json")).unwrap(), bytes);
    let held = resource::lock(dir.path()).unwrap();
    settings["cpu"] = json!(4);
    let (status, _, _) = response(
        app.clone(),
        request("PUT", path, Some(id), &settings.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = response(
        app,
        request("POST", "/api/service/resources/apply", Some(id), "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    drop(held);
}

#[tokio::test]
async fn untrusted_hosts_origins_and_controller_ids_cannot_mutate_settings() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_for(dir.path().to_owned(), resource::tests::platform());
    let path = "/api/service/resources";
    for (header, value) in [
        ("host", "evil.example:5524"),
        ("origin", "https://evil.example"),
    ] {
        let mut req = request("GET", path, None, "");
        req.headers_mut()
            .insert(header, HeaderValue::from_str(value).unwrap());
        assert_eq!(response(app.clone(), req).await.0, StatusCode::FORBIDDEN);
    }
    for id in [None, Some("another-host")] {
        let (status, headers, _) = response(app.clone(), request("PUT", path, id, "{}")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(headers["cache-control"], "no-store");
    }
    let (status, headers, _) = response(app.clone(), request("OPTIONS", path, None, "")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        headers["access-control-allow-methods"],
        "GET, PUT, POST, OPTIONS"
    );
    assert!(
        headers["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("X-Firecrab-Controller-Id")
    );
    assert!(!dir.path().join("resources.json").exists());
    fs::write(
        dir.path().join("runtime/resource-controller-id.json"),
        b"corrupt",
    )
    .unwrap();
    assert_eq!(
        response(app, request("PUT", path, Some("stale"), "{}"))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn invalid_bodies_and_broken_state_return_errors_without_starting_a_worker() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::tests::platform();
    let app = router_for(dir.path().to_owned(), platform);
    let id = resource::controller_id(dir.path()).unwrap();
    let path = "/api/service/resources";
    assert_eq!(
        response(
            app.clone(),
            request(
                "POST",
                "/api/service/resources/apply",
                Some(&id),
                "{\"unexpected\":true}"
            )
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        response(
            app.clone(),
            request("PUT", path, Some(&id), &" ".repeat(4097))
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        response(app.clone(), request("PUT", path, Some(&id), "{broken}"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert!(!dir.path().join("runtime/resource-operation.json").exists());
    fs::write(dir.path().join("resources.json"), b"broken").unwrap();
    assert_eq!(
        response(app, request("GET", path, None, "")).await.0,
        StatusCode::BAD_REQUEST
    );
    let offline = resource::Platform {
        capacity: |_| Err(Error::Invalid("capacity probe failed".into())),
        ..platform
    };
    let (status, _, body) = response(
        router_for(dir.path().to_owned(), offline),
        request("GET", path, None, ""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "capacity probe failed");
}

#[tokio::test]
async fn accepted_apply_is_persistent_and_blocks_duplicate_requests() {
    let dir = tempfile::tempdir().unwrap();
    let app = router_for(dir.path().to_owned(), resource::tests::platform());
    let id = resource::controller_id(dir.path()).unwrap();
    let path = "/api/service/resources/apply";
    #[cfg(unix)]
    let mut command = Command::new("/bin/sh");
    #[cfg(unix)]
    command.args(["-c", "exit 0"]);
    #[cfg(windows)]
    let mut command = Command::new("cmd.exe");
    #[cfg(windows)]
    command.args(["/C", "exit /B 0"]);
    let operation = spawn_worker(dir.path(), resource::tests::platform(), command).unwrap();
    assert_eq!(operation.state, "applying");
    assert!(uuid::Uuid::parse_str(&operation.id).is_ok());
    assert!(dir.path().join("runtime/resource-apply.log").exists());
    let stored: ManagerResourceOperation =
        resource::read(&dir.path().join("runtime/resource-operation.json"))
            .unwrap()
            .unwrap();
    assert_eq!(stored.id, operation.id);
    assert_eq!(
        response(app, request("POST", path, Some(&id), "{}"))
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn status_exposes_unsupported_wsl_networking_without_replacing_saved_resources() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::Platform {
        os: firecrab_api_types::HostOs::Windows,
        defaults: |_, _| {
            Err(Error::Invalid(
                "WSL network mode virtioproxy is not supported by resource management".into(),
            ))
        },
        ..resource::tests::platform()
    };
    let app = router_for(dir.path().to_owned(), platform);
    let (status, _, body) = response(
        app.clone(),
        request("GET", "/api/service/resources", None, ""),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("virtioproxy"));
    let saved = resource::tests::platform()
        .status(dir.path())
        .unwrap()
        .settings;
    resource::write(&dir.path().join("resources.json"), &saved).unwrap();
    let (status, _, body) = response(app, request("GET", "/api/service/resources", None, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["settings"]["cpu"], saved.cpu);
    assert!(body["sharedWsl"].as_bool().unwrap());
}

#[test]
fn worker_spawn_failure_is_recorded_and_allows_retry() {
    let dir = tempfile::tempdir().unwrap();
    let platform = resource::tests::platform();
    let error = spawn_worker(
        dir.path(),
        platform,
        Command::new(dir.path().join("missing-executable")),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Io(_)));
    let status = platform.status(dir.path()).unwrap();
    assert_eq!(status.operation.unwrap().state, "failed");
    assert!(status.pending);
    assert!(platform.save(dir.path(), &status.settings).is_ok());
}

#[test]
fn resource_server_rejects_relative_and_missing_homes() {
    assert!(run(Some(PathBuf::from("relative-home"))).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(run(Some(dir.path().join("missing"))).is_err());
}
