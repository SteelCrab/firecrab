use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::server::{HttpConfig, build_router};
use crate::templates::TemplateRegistry;

async fn state(root: &std::path::Path) -> AppState {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap()
}

async fn app(root: &std::path::Path) -> Router {
    let state = state(root).await;
    state.store.put_pipe_auth(&github("team")).unwrap();
    router(state)
}

fn github(username: &str) -> SavedAuth {
    SavedAuth::Github {
        username: username.into(),
        token: "test-only-secret".into(),
    }
}

fn router(state: AppState) -> Router {
    let config = HttpConfig {
        bind_addr: "127.0.0.1:3000".parse().unwrap(),
        allowed_origins: vec![],
        max_concurrent_requests: 128,
        request_timeout: std::time::Duration::from_secs(10),
        static_root: None,
    };
    build_router(state, &config)
}

async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

fn minimal(name: &str) -> Value {
    json!({"name": name, "image": "ghcr.io/team/orders:1.0", "port": 8080, "ram": 512, "cpu": 1})
}

#[tokio::test]
async fn micro_services_crud_survives_restart_and_clears_optional_fields() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    let mut spec = minimal("orders");
    let secret = Uuid::new_v4();
    spec["healthPath"] = json!("/actuator/health");
    spec["domain"] = json!("orders.example.com");
    spec["replicas"] = json!(3);
    spec["startupTimeout"] = json!(90);
    spec["drainTimeout"] = json!(45);
    spec["env"] = json!({"SPRING_PROFILES_ACTIVE": "prod", "EMPTY": ""});
    spec["secrets"] = json!({"DB_PASSWORD": secret});
    let (status, created) = request(&router, "POST", "/api/micro-services", spec.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["runtime"], "vm");
    assert_eq!(created["secrets"]["DB_PASSWORD"], secret.to_string());
    assert!(created.get("spec").is_none());
    let typed: MicroServiceResponse = serde_json::from_value(created.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), created);
    let path = format!("/api/micro-services/{}", created["id"].as_str().unwrap());
    drop(router);
    let router = app(directory.path()).await;
    assert_eq!(
        request(&router, "GET", &path, Value::Null).await,
        (StatusCode::OK, created.clone())
    );
    assert_eq!(
        request(&router, "GET", "/api/micro-services", Value::Null)
            .await
            .1,
        json!([created])
    );

    let mut replacement = minimal("orders-v2");
    replacement["image"] = json!(format!("ghcr.io/team/orders@sha256:{}", "a".repeat(64)));
    replacement["drainTimeout"] = json!(0);
    let (status, updated) = request(&router, "PUT", &path, replacement).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["env"], json!({}));
    assert_eq!(updated["secrets"], json!({}));
    assert!(updated["domain"].is_null());
    assert_eq!(updated["healthPath"], "/health");
    assert_eq!(updated["startupTimeout"], 300);
    assert_eq!(updated["replicas"], 1);
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
    drop(router);
    let router = app(directory.path()).await;
    for method in ["GET", "PUT", "DELETE"] {
        assert_eq!(
            request(&router, method, &path, minimal("orders")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        request(&router, "GET", "/api/micro-services", Value::Null)
            .await
            .1,
        json!([])
    );
}

#[tokio::test]
async fn micro_services_duplicate_and_invalid_updates_leave_previous_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    let (first, second) = tokio::join!(
        request(&router, "POST", "/api/micro-services", minimal("orders")),
        request(&router, "POST", "/api/micro-services", minimal("orders")),
    );
    assert!(
        (first.0 == StatusCode::CREATED && second.0 == StatusCode::BAD_REQUEST)
            || (second.0 == StatusCode::CREATED && first.0 == StatusCode::BAD_REQUEST)
    );
    let (_, billing) = request(&router, "POST", "/api/micro-services", minimal("billing")).await;
    let path = format!("/api/micro-services/{}", billing["id"].as_str().unwrap());
    let (status, error) = request(&router, "PUT", &path, minimal("orders")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["fields"]["name"], "is already registered");
    let mut invalid = minimal("billing");
    invalid["cpu"] = json!(0);
    assert_eq!(
        request(&router, "PUT", &path, invalid).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(request(&router, "GET", &path, Value::Null).await.1, billing);
    let rows = request(&router, "GET", "/api/micro-services", Value::Null)
        .await
        .1;
    assert_eq!(rows[0]["name"], "billing");
    assert_eq!(rows[1]["name"], "orders");
}

#[tokio::test]
async fn micro_services_reject_invalid_fields_and_secret_value_ambiguity() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    let cases = [
        ("name", json!("bad_name")),
        ("name", json!("-orders")),
        ("name", json!("Orders")),
        ("name", json!("a".repeat(64))),
        ("image", json!("ghcr.io/team/orders:bad tag")),
        ("image", json!("ghcr.io/team/orders@sha256:abc")),
        ("port", json!(0)),
        ("ram", json!(513)),
        ("ram", json!(65536)),
        ("cpu", json!(33)),
        ("replicas", json!(0)),
        ("replicas", json!(101)),
        ("startupTimeout", json!(0)),
        ("startupTimeout", json!(3601)),
        ("drainTimeout", json!(3601)),
        ("healthPath", json!("http://host/health")),
        ("healthPath", json!("//host/health")),
        ("healthPath", json!("/health#fragment")),
        ("healthPath", json!("/health\n")),
        ("domain", json!("https://orders.example.com")),
        ("domain", json!("")),
        ("domain", json!("orders.example.com:443")),
        ("domain", json!("*.example.com")),
        ("env", json!({"BAD-NAME": "value"})),
        ("env", json!({"OK": "nul\0value"})),
        ("secrets", json!({"BAD-NAME": Uuid::new_v4()})),
        ("secrets", json!({"DB_PASSWORD": Uuid::nil()})),
    ];
    for (field, value) in cases {
        let mut spec = minimal("orders");
        spec[field] = value;
        let (status, error) = request(&router, "POST", "/api/micro-services", spec).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {error}");
        assert!(
            error["error"]["fields"].get(field).is_some(),
            "{field}: {error}"
        );
    }
    let mut spec = minimal("orders");
    spec["env"] = json!({"DB_PASSWORD": "value"});
    spec["secrets"] = json!({"DB_PASSWORD": Uuid::new_v4()});
    assert_eq!(
        request(&router, "POST", "/api/micro-services", spec)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut spec = minimal("orders");
    spec["env"] = serde_json::to_value(
        (0..64)
            .map(|i| (format!("VAR_{i}"), "x"))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    spec["secrets"] = json!({"DB_PASSWORD": Uuid::new_v4()});
    assert_eq!(
        request(&router, "POST", "/api/micro-services", spec)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&router, "GET", "/api/micro-services", Value::Null)
            .await
            .1,
        json!([])
    );
}

#[tokio::test]
async fn micro_services_reject_unknown_fields_runtime_and_malformed_ids() {
    let directory = tempfile::tempdir().unwrap();
    let router = app(directory.path()).await;
    for (field, value) in [
        ("runtime", json!("container")),
        ("secrets", json!({"DB_PASSWORD": "secret-value"})),
        ("startupTimout", json!(123)),
    ] {
        let mut spec = minimal("orders");
        spec[field] = value;
        let (status, error) = request(&router, "POST", "/api/micro-services", spec).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid_json");
    }
    assert_eq!(
        request(&router, "GET", "/api/micro-services/bad-id", Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn micro_services_auth_scopes_create_update_and_deploy_to_connected_accounts() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    let router = router(state.clone());
    // A standalone login must never substitute for Pipe Auth, including public images.
    state
        .store
        .put_registry_login(
            &crate::oci::RegistryCredential {
                registry: "ghcr.io".into(),
                username: "team".into(),
                secret: "standalone-secret".into(),
            },
            None,
        )
        .unwrap();
    let (status, error) = request(&router, "POST", "/api/micro-services", minimal("orders")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(error["error"]["fields"]["image"].is_string());
    assert!(!error.to_string().contains("standalone-secret"));
    assert!(state.store.list_micro_services().unwrap().is_empty());

    state.store.put_pipe_auth(&github("Team")).unwrap();
    let (status, saved) = request(&router, "POST", "/api/micro-services", minimal("orders")).await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    let id = saved["id"].as_str().unwrap();
    let path = format!("/api/micro-services/{id}");
    for image in [
        "ghcr.io/other/orders:1.0",
        "ghcr.io/team-evil/orders:1.0",
        "ghcr.io.evil.test/team/orders:1.0",
        "ghcr.io:443/team/orders:1.0",
        "nginx:1.27",
        "docker.io/team/orders:1.0",
        "127.0.0.1:5000/team/orders:1.0",
    ] {
        let mut changed = minimal("orders");
        changed["image"] = json!(image);
        for (method, target) in [("POST", "/api/micro-services"), ("PUT", path.as_str())] {
            let (status, error) = request(&router, method, target, changed.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{image}: {error}");
            assert!(
                error["error"]["fields"]["image"].is_string(),
                "{image}: {error}"
            );
        }
        assert_eq!(request(&router, "GET", &path, Value::Null).await.1, saved);
    }
    let (status, _) = request(&router, "DELETE", "/api/pipe/auth/github", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    for method in ["PUT", "POST"] {
        let target = if method == "PUT" {
            path.clone()
        } else {
            format!("{path}/deployment")
        };
        let body = if method == "PUT" {
            minimal("orders")
        } else {
            json!({"hostPort": 30100})
        };
        let (status, error) = request(&router, method, &target, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"]["fields"]["image"].is_string());
    }
    assert!(
        state
            .store
            .service_deployment(Uuid::parse_str(id).unwrap())
            .unwrap()
            .is_none()
    );
    // Removing AUTH does not strand a saved configuration: reads and deletion remain available.
    assert_eq!(request(&router, "GET", &path, Value::Null).await.1, saved);
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null).await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn micro_services_auth_accepts_only_the_connected_ecr_account_and_region() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path()).await;
    state
        .store
        .put_pipe_auth(&SavedAuth::Aws {
            account_id: "123456789012".into(),
            credentials: crate::pipe_auth::AwsCredentials {
                access_key_id: "test-access-key".into(),
                secret_access_key: "aws-private-secret".into(),
                session_token: String::new(),
                region: "ap-northeast-2".into(),
            },
        })
        .unwrap();
    let router = router(state.clone());
    let host = "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com";
    for (index, image) in [
        format!("{host}/orders:1.0"),
        format!("{host}/nested/orders@sha256:{}", "a".repeat(64)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut body = minimal(&format!("orders-{index}"));
        body["image"] = json!(image);
        let (status, response) = request(&router, "POST", "/api/micro-services", body).await;
        assert_eq!(status, StatusCode::CREATED, "{response}");
        assert!(!response.to_string().contains("aws-private-secret"));
    }
    for image in [
        "999999999999.dkr.ecr.ap-northeast-2.amazonaws.com/orders:v1",
        "123456789012.dkr.ecr.us-east-1.amazonaws.com/orders:v1",
        "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com.evil.test/orders:v1",
        "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com:443/orders:v1",
        "public.ecr.aws/team/orders:v1",
        "ghcr.io/team/orders:v1",
    ] {
        let mut body = minimal("rejected");
        body["image"] = json!(image);
        let (status, error) = request(&router, "POST", "/api/micro-services", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{image}: {error}");
        assert!(error["error"]["fields"]["image"].is_string());
        assert!(!error.to_string().contains("aws-private-secret"));
    }
    assert_eq!(state.store.list_micro_services().unwrap().len(), 2);
}

#[tokio::test]
async fn deployment_check_reports_missing_network_and_replica_port_without_creating_resources() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path()).await;
    let id = Uuid::new_v4();
    let spec: MicroServiceSpec = serde_json::from_value(json!({
        "name": "orders", "image": "ghcr.io/team/orders:1.0", "port": 8080,
        "ram": 512, "cpu": 1, "replicas": 2
    }))
    .unwrap();
    state
        .store
        .insert_micro_service(&MicroServiceResponse { id, spec })
        .unwrap();
    let router = router(state.clone());
    let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let occupied = listener.local_addr().unwrap().port();
    let first = occupied - 1;
    let path = format!("/api/micro-services/{id}/deployment/check");
    let (status, error) = request(
        &router,
        "POST",
        &path,
        json!({
            "hostPort": first, "microNetworkId": Uuid::new_v4()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"]["fields"]["microNetworkId"],
        "network does not exist"
    );
    assert!(
        error["error"]["fields"]["hostPort"]
            .as_str()
            .unwrap()
            .contains("already used")
    );
    assert!(state.store.service_deployment(id).unwrap().is_none());
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    assert!(state.vms.lock().unwrap().is_empty());

    drop(listener);
    assert_eq!(
        request(&router, "POST", &path, json!({"hostPort": occupied})).await,
        (StatusCode::OK, Value::Null)
    );
    assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, occupied)).is_ok());
    assert!(state.store.service_deployment(id).unwrap().is_none());
    assert_eq!(
        request(
            &router,
            "POST",
            &format!("/api/micro-services/{}/deployment/check", Uuid::new_v4()),
            json!({"hostPort": occupied})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn deploy_rechecks_port_availability_after_a_successful_check() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path()).await;
    state.store.put_pipe_auth(&github("team")).unwrap();
    let router = router(state.clone());
    let (_, service) = request(&router, "POST", "/api/micro-services", minimal("orders")).await;
    let id = service["id"].as_str().unwrap();
    let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let path = format!("/api/micro-services/{id}/deployment");
    assert_eq!(
        request(
            &router,
            "POST",
            &format!("{path}/check"),
            json!({"hostPort": port})
        )
        .await
        .0,
        StatusCode::OK
    );
    let _listener = std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).unwrap();
    let (status, error) = request(&router, "POST", &path, json!({"hostPort": port})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        error["error"]["fields"]["hostPort"]
            .as_str()
            .unwrap()
            .contains(&port.to_string())
    );
    assert!(
        state
            .store
            .service_deployment(Uuid::parse_str(id).unwrap())
            .unwrap()
            .is_none()
    );
}
