use super::*;
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use serde_json::{Value, json};

#[tokio::test]
async fn invalid_config_credentials_and_oversized_requests_fail_before_helper_calls() {
    let config =
        json!({"routes":[{"host":"app.example.com","upstreams":["127.0.0.1:9000"]}],"tunnel":null});
    let mut invalid_config = config.clone();
    invalid_config["listen"] = json!("0.0.0.0:8080");
    let mut tunnel_config = config.clone();
    tunnel_config["tunnel"] =
        json!({"id":"11111111-1111-4111-8111-111111111111","credentialsFile":"credentials.json"});
    let oversized = json!({"AccountTag":"account","TunnelID":"11111111-1111-4111-8111-111111111111","TunnelSecret":"private-fixture-secret","extra":"x".repeat(48*1024)});
    for (config, credentials, field) in [
        (invalid_config, Value::Null, "config"),
        (config, json!({}), "credentials"),
        (
            tunnel_config.clone(),
            json!({"TunnelID":"22222222-2222-4222-8222-222222222222"}),
            "credentials",
        ),
        (tunnel_config, oversized, "config"),
    ] {
        let request =
            serde_json::from_value(json!({"config":config,"credentials":credentials})).unwrap();
        let request_id = RequestId(uuid::Uuid::new_v4());
        let response = apply(Extension(request_id), Json(request))
            .await
            .unwrap_err()
            .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(body["error"]["fields"][field].is_string(), "{body}");
        assert!(!body.to_string().contains("private-fixture-secret"));
    }
}
