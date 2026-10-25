use super::*;
use crate::secrets::{ConnectionSummary, Settings};

fn connection(base: &str) -> Connection {
    Connection {
        summary: ConnectionSummary {
            id: uuid::Uuid::new_v4(),
            name: "test".into(),
            provider: Provider::Infisical,
            settings: Settings {
                base_url: base.into(),
                project_id: "project".into(),
                environment: "prod".into(),
                secret_path: "/".into(),
                ..Default::default()
            },
            version: 1,
            updated_at: 0,
        },
        credentials: BTreeMap::from([("token".into(), "fixture-token".into())]),
    }
}

#[test]
fn provider_urls_require_tls_except_for_loopback_and_refuse_embedded_credentials() {
    for url in [
        "http://example.com",
        "https://user:password@example.com",
        "https://example.com?token=secret",
        "file:///tmp/secrets",
        "https://example.com#fragment",
    ] {
        assert!(base_url(url).is_err(), "accepted {url}");
    }
    for url in [
        "https://secrets.internal",
        "http://127.0.0.1:9000",
        "http://[::1]:9000",
        "http://localhost:9000",
    ] {
        assert!(base_url(url).is_ok(), "rejected {url}");
    }
}

#[tokio::test]
async fn infisical_reads_values_checks_scope_and_refuses_hidden_values() {
    use axum::{Json, Router, extract::Query, http::HeaderMap, routing::get};
    let server = Router::new().route("/api/v4/secrets", get(|headers: HeaderMap, Query(query): Query<BTreeMap<String, String>>| async move {
        assert_eq!(headers["authorization"], "Bearer fixture-token");
        assert_eq!(query["projectId"], "project");
        assert_eq!(query["environment"], "prod");
        assert_eq!(query["expandSecretReferences"], "false");
        Json(json!({"secrets":[{"secretKey":"PASSWORD","secretValue":"remote-value","secretValueHidden":false}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let values = list(&connection(&format!("http://{address}")))
        .await
        .unwrap();
    assert_eq!(values["PASSWORD"], "remote-value");
    task.abort();

    let server = Router::new().route("/api/v4/secrets", get(|| async { Json(json!({"secrets":[{"secretKey":"PASSWORD","secretValue":"","secretValueHidden":true}]})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    assert!(matches!(
        list(&connection(&format!("http://{address}"))).await,
        Err(SecretError::Invalid(_))
    ));
    task.abort();
}

#[tokio::test]
async fn infisical_creates_and_updates_scoped_multiline_values_with_encoded_names() {
    use axum::{
        Json, Router,
        extract::Path,
        http::{HeaderMap, Method},
        routing::post,
    };
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let receive = move |method: Method,
                        Path(name): Path<String>,
                        headers: HeaderMap,
                        Json(body): Json<Value>| {
        let sent = sent.clone();
        async move {
            sent.send((method, name, headers, body)).unwrap();
            Json(json!({}))
        }
    };
    let server = Router::new().route(
        "/api/v4/secrets/{name}",
        post(receive.clone()).patch(receive),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let mut connection = connection(&format!("http://{address}"));
    connection.summary.settings.secret_path = "/services/api".into();
    let value = "first line\n${REFERENCE}\nlast line";
    for (previous, expected_method) in [(None, Method::POST), (Some("old"), Method::PATCH)] {
        upload(&connection, "prod/db", value, previous)
            .await
            .unwrap();
        let (method, name, headers, body) = received.try_recv().unwrap();
        assert_eq!(method, expected_method);
        assert_eq!(name, "prod/db");
        assert_eq!(headers["authorization"], "Bearer fixture-token");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(
            body,
            json!({
                "projectId": "project",
                "environment": "prod",
                "secretPath": "/services/api",
                "secretValue": value,
                "type": "shared",
                "skipMultilineEncoding": true,
            })
        );
    }
    task.abort();
}

#[tokio::test]
async fn infisical_rejects_duplicate_names_instead_of_silently_overwriting_values() {
    use axum::{Json, Router, routing::get};
    let server = Router::new().route(
        "/api/v4/secrets",
        get(|| async {
            Json(json!({"secrets": [
                {"secretKey": "KEY", "secretValue": "first"},
                {"secretKey": "KEY", "secretValue": "second"},
            ]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    assert!(matches!(
        list(&connection(&format!("http://{address}"))).await,
        Err(SecretError::Provider)
    ));
    task.abort();
}

#[tokio::test]
async fn redirects_and_provider_errors_do_not_leak_tokens_or_follow_redirects() {
    use axum::{Router, response::Redirect, routing::get};
    let server = Router::new().route(
        "/api/v4/secrets",
        get(|| async { Redirect::temporary("http://127.0.0.1:1/private-provider-token") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let error = list(&connection(&format!("http://{address}")))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), SecretError::Provider.to_string());
    task.abort();
}

#[test]
fn aws_prefix_accepts_only_literal_secret_name_characters() {
    let mut target = connection("");
    target.summary.provider = Provider::AwsSecretsManager;
    target.summary.settings.region = "ap-northeast-2".into();
    target.credentials = BTreeMap::from([
        ("accessKeyId".into(), "fixture-access".into()),
        ("secretAccessKey".into(), "fixture-secret".into()),
    ]);
    for prefix in ["!prod/", "prod*", "prod dev", "prod\n", &"a".repeat(513)] {
        target.summary.settings.prefix = prefix.into();
        assert!(validate(&target).is_err(), "accepted unsafe AWS prefix");
    }
    for prefix in ["", "prod/", "DEV_DB", "_./+=@-", &"a".repeat(512)] {
        target.summary.settings.prefix = prefix.into();
        assert!(validate(&target).is_ok(), "rejected literal AWS prefix");
    }
}

#[test]
fn aws_signing_is_stable_for_json_payloads_and_session_credentials() {
    let headers = BTreeMap::from([
        ("content-type", "application/x-amz-json-1.1".into()),
        ("host", "secretsmanager.ap-northeast-2.amazonaws.com".into()),
        ("x-amz-date", "20261021T030000Z".into()),
        ("x-amz-security-token", "SESSION".into()),
        ("x-amz-target", "secretsmanager.GetSecretValue".into()),
    ]);
    let signed = sign_aws(
        "ap-northeast-2",
        "AKIDEXAMPLE",
        "example-secret",
        "20261021T030000Z",
        &headers,
        "{\"SecretId\":\"prod/db\"}",
    );
    assert!(signed.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/ap-northeast-2/secretsmanager/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature="));
    assert!(
        signed.ends_with(
            "Signature=38b9af44a76a073faf2bfbeae9700a42bfe1b08bdcf0183b8595aa617d4e855d"
        )
    );
}

#[tokio::test]
async fn firecrab_refuses_a_target_that_rotates_between_the_final_list_and_write() {
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    let id = uuid::Uuid::new_v4();
    let server = Router::new()
        .route(
            "/api/secrets",
            get(move || async move {
                Json(json!([{"id":id,"name":"KEY","description":"keep","version":1}]))
            }),
        )
        .route(
            &format!("/api/secrets/{id}/reveal"),
            post(|| async { Json(json!({"value":"rotated-value","version":2})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, server).await.unwrap();
    });
    let mut connection = connection(&format!("http://{address}"));
    connection.summary.provider = Provider::Firecrab;
    let result = upload(&connection, "KEY", "replacement", Some("previous-value")).await;
    assert!(matches!(result, Err(SecretError::Conflict)));
    task.abort();
}
