use super::*;
use axum::extract::ConnectInfo;

async fn as_host(
    f: &Fixture,
    host: Uuid,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5523")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(crate::tls::AuthenticatedHost { host_id: host }));
    let response = f.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn set_policy(f: &Fixture, host: Uuid, project: Uuid, permissions: &[&str]) {
    let (status, body) = call(
        &f.app,
        "PUT",
        &format!("/api/ai-sandbox/access/{host}"),
        json!({
            "grants": [{"resource":{"kind":"project","id":project},"permissions":permissions}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn canvas_messages_enforce_grants_and_deduplicate_retries() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    let viewer = Uuid::new_v4();
    let editor = Uuid::new_v4();
    set_policy(&f, viewer, project, &["read"]).await;
    set_policy(&f, editor, project, &["read", "write"]).await;
    let base = format!("/api/ai-sandbox/projects/{project}/canvas/messages");
    let id = Uuid::new_v4();
    let target = format!("{base}/{id}");
    assert_eq!(
        as_host(&f, viewer, "PUT", &target, json!({"text":"hello"}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = as_host(&f, editor, "PUT", &target, json!({"text":"hello"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["author"]["host"], editor.to_string());
    assert_eq!(
        as_host(&f, editor, "PUT", &target, json!({"text":"hello"})).await,
        (StatusCode::OK, saved.clone())
    );
    assert_eq!(
        as_host(&f, editor, "PUT", &target, json!({"text":"different"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&f.app, "PUT", &target, json!({"text":"hello"}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (status, messages) = as_host(&f, viewer, "GET", &base, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        messages["messages"].as_array().unwrap(),
        std::slice::from_ref(&saved)
    );
    assert_eq!(messages["canWrite"], false);
    let cursor = saved["sequence"].as_u64().unwrap();
    assert_eq!(
        as_host(
            &f,
            viewer,
            "GET",
            &format!("{base}?after={cursor}"),
            Value::Null
        )
        .await
        .1,
        json!({"messages":[], "canWrite":false})
    );
    assert_eq!(
        call(
            &f.app,
            "DELETE",
            &format!("/api/ai-sandbox/access/{viewer}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        as_host(&f, viewer, "GET", &base, Value::Null).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("{base}/{}", Uuid::new_v4()),
            json!({"text":"  "})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("{base}/{}", Uuid::new_v4()),
            json!({"text":"sandbox-private-key"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn canvas_messages_page_and_project_deletion_do_not_leak_data() {
    let f = fixture().await;
    let connection = connect(&f, "openai").await;
    verify(&f, connection, false).await;
    let project = project(&f, connection, false).await;
    let other = super::project(&f, connection, false).await;
    for index in 0..205 {
        f.state
            .store
            .save_canvas_message(
                project,
                Uuid::new_v4(),
                Default::default(),
                &format!("message {index}"),
                1,
            )
            .unwrap();
    }
    let base = format!("/api/ai-sandbox/projects/{project}/canvas/messages");
    let first = call(&f.app, "GET", &base, Value::Null).await.1;
    assert_eq!(first["messages"].as_array().unwrap().len(), 200);
    let after = first["messages"][199]["sequence"].as_u64().unwrap();
    assert_eq!(
        call(&f.app, "GET", &format!("{base}?after={after}"), Value::Null)
            .await
            .1["messages"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        call(
            &f.app,
            "GET",
            &format!("/api/ai-sandbox/projects/{other}/canvas/messages"),
            Value::Null
        )
        .await
        .1,
        json!({"messages":[], "canWrite":true})
    );
    let spec = serde_json::to_value(
        f.state
            .store
            .sandbox_project(project)
            .unwrap()
            .unwrap()
            .spec,
    )
    .unwrap();
    assert_eq!(
        call(
            &f.app,
            "DELETE",
            &format!("/api/ai-sandbox/projects/{project}"),
            Value::Null
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &f.app,
            "PUT",
            &format!("/api/ai-sandbox/projects/{project}"),
            spec
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&f.app, "GET", &base, Value::Null).await.1,
        json!({"messages":[], "canWrite":true})
    );
    let (_, new_message) = call(
        &f.app,
        "PUT",
        &format!("{base}/{}", Uuid::new_v4()),
        json!({"text":"new project"}),
    )
    .await;
    assert_eq!(new_message["sequence"], 206);
    let other_base = format!("/api/ai-sandbox/projects/{other}/canvas/messages");
    let (_, other_message) = call(
        &f.app,
        "PUT",
        &format!("{other_base}/{}", Uuid::new_v4()),
        json!({"text":"other project"}),
    )
    .await;
    assert_eq!(other_message["sequence"], 1);
}
