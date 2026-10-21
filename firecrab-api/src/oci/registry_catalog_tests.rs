use super::*;
use axum::{
    Json, Router,
    extract::Query,
    http::HeaderMap,
    routing::{get, post},
};
use base64::Engine;
use std::collections::HashMap;

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, task)
}

fn credential(registry: &str) -> RegistryCredential {
    RegistryCredential {
        registry: registry.into(),
        username: "octocat".into(),
        secret: "private-token".into(),
    }
}

#[tokio::test]
async fn github_lists_private_user_packages_and_pages_in_the_selected_namespace() {
    let app = Router::new().route(
        "/user/packages",
        get(
            |headers: HeaderMap, Query(query): Query<HashMap<String, String>>| async move {
                assert_eq!(headers["authorization"], "Bearer private-token");
                assert_eq!(query["package_type"], "container");
                assert_eq!(query["per_page"], "100");
                assert_eq!(query["page"], "2");
                (
                    [("link", "<https://untrusted.invalid/?page=3>; rel=\"next\"")],
                    Json(serde_json::json!([{"name":"nested/app"},{"name":"api"}])),
                )
            },
        ),
    );
    let (base, task) = serve(app).await;
    let result = github_repositories(
        &Client::new(),
        &base,
        "Octocat",
        2,
        Some(&credential("ghcr.io")),
    )
    .await
    .unwrap();
    assert_eq!(result.repositories, ["octocat/api", "octocat/nested/app"]);
    assert_eq!(result.next_page, Some(3));
    task.abort();
}

#[tokio::test]
async fn github_uses_the_organization_catalog_when_the_owner_is_an_organization() {
    let app = Router::new()
        .route(
            "/users/team",
            get(|| async { Json(serde_json::json!({"type":"Organization"})) }),
        )
        .route(
            "/orgs/team/packages",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer private-token");
                // A previous page can be present without there being a next page.
                (
                    [("link", "<https://api.github.com/?page=1>; rel=\"prev\"")],
                    Json(serde_json::json!([{"name":"app"}])),
                )
            }),
        );
    let (base, task) = serve(app).await;
    let result = github_repositories(
        &Client::new(),
        &base,
        "team",
        2,
        Some(&credential("ghcr.io")),
    )
    .await
    .unwrap();
    assert_eq!(result.repositories, ["team/app"]);
    assert_eq!(result.next_page, None);
    task.abort();
}

#[tokio::test]
async fn docker_lists_the_saved_accounts_repositories_and_does_not_follow_pagination_urls() {
    let app = Router::new()
        .route("/v2/auth/token", post(|Json(body): Json<serde_json::Value>| async move {
            assert_eq!(body, serde_json::json!({"identifier":"octocat","secret":"private-token"}));
            Json(serde_json::json!({"access_token":"hub-token"}))
        }))
        .route("/v2/namespaces/octocat/repositories", get(|headers: HeaderMap, Query(query): Query<HashMap<String, String>>| async move {
            assert_eq!(headers["authorization"], "Bearer hub-token");
            assert_eq!(query["page"], "1");
            assert_eq!(query["page_size"], "100");
            Json(serde_json::json!({"results":[{"name":"app"}],"next":"https://untrusted.invalid/leak"}))
        }));
    let (base, task) = serve(app).await;
    let result = docker_repositories(&Client::new(), &base, 1, &credential("docker.io"))
        .await
        .unwrap();
    assert_eq!(result.repositories, ["octocat/app"]);
    assert_eq!(result.next_page, Some(2));
    task.abort();
}

#[tokio::test]
async fn docker_requires_a_saved_login_instead_of_listing_official_images() {
    for registry in ["docker.io", "registry-1.docker.io"] {
        let error = repositories(registry, "library", 1, None)
            .await
            .unwrap_err();
        assert!(matches!(error, ResolveError::Authentication(_)));
    }
}

#[tokio::test]
async fn docker_does_not_expose_authentication_error_bodies() {
    let app = Router::new().route(
        "/v2/auth/token",
        post(|| async { (StatusCode::UNAUTHORIZED, "do not expose private-token") }),
    );
    let (base, task) = serve(app).await;
    let error = docker_repositories(&Client::new(), &base, 1, &credential("docker.io"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("401"));
    assert!(!error.contains("private-token"));
    task.abort();
}

#[tokio::test]
async fn ecr_passwords_report_unsupported_repository_discovery() {
    let result = repositories("public.ecr.aws", "", 1, None).await.unwrap();
    assert!(!result.listing_supported);
    assert!(result.repositories.is_empty());
    assert_eq!(result.next_page, None);
}

#[tokio::test]
async fn harbor_registry_lists_authorized_repositories_without_prefixing_the_username() {
    let app = Router::new().route("/api/v2.0/repositories", get(|headers: HeaderMap, Query(query): Query<HashMap<String, String>>| async move {
        let authorization = headers["authorization"].to_str().unwrap();
        let encoded = authorization.strip_prefix("Basic ").unwrap();
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        assert_eq!(decoded, b"octocat:private-token");
        assert_eq!(query["page"], "1");
        assert_eq!(query["page_size"], "100");
        ([("x-total-count", "101")], Json(serde_json::json!([{"name":"firecrab/harbor-nginx"},{"name":"team/nested/app"}])))
    }));
    let (base, task) = serve(app).await;
    let registry = base.trim_start_matches("http://");
    let result = repositories(registry, "octocat", 1, Some(credential(registry)))
        .await
        .unwrap();
    assert_eq!(
        result.repositories,
        ["firecrab/harbor-nginx", "team/nested/app"]
    );
    assert_eq!(result.next_page, Some(2));
    assert!(result.listing_supported);
    task.abort();
}

#[tokio::test]
async fn harbor_registry_requires_an_exact_saved_login() {
    assert!(
        repositories("harbor.example.com", "", 1, Some(credential("ghcr.io")))
            .await
            .is_err()
    );
}

#[test]
fn rejects_unsafe_namespace_and_repository_names() {
    for value in [
        "",
        "../user",
        "user?query",
        "a/b",
        "a.b",
        "-name",
        "https://host",
    ] {
        assert!(!valid_namespace(value), "{value}");
    }
    for name in ["../app", "app?query", "app:tag", "app#fragment", ""] {
        assert!(
            catalog("team", vec![name.into()], false, 1).is_err(),
            "{name}"
        );
    }
}
