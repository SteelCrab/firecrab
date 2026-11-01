use super::*;
const REPOS: &str = "/api/code/github/repositories?account=octocat";
const BRANCHES: &str = "/api/code/github/branches?account=octocat&owner=team&repository=web-app";
fn repo(n: usize) -> Value {
    json!({"owner":{"login":"team"},"name":format!("app-{n}"),"full_name":format!("team/app-{n}"),"default_branch":"main","private":true,"disabled":false,"language":"Python","secret":"code-private-extra"})
}
fn reply(f: &Fixture, path: &str, status: u16, value: Value) {
    f.remote
        .replies
        .lock()
        .unwrap()
        .insert(path.into(), (status, value));
}
async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("host", "127.0.0.1:5523")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("code-private"));
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn code_catalog_paginates_sanitizes_and_reuses_only_the_github_credential() {
    let f = fixture().await;
    reply(
        &f,
        "/github/user/repos",
        200,
        json!((0..20).map(repo).collect::<Vec<_>>()),
    );
    let (status, page) = get(&f.app, &format!("{REPOS}&page=2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["account"], "octocat");
    assert_eq!(page["nextPage"], 3);
    let public: CodeRepositoryPage = serde_json::from_value(page.clone()).unwrap();
    assert_eq!(public.repositories.len(), 20);
    assert_eq!(page["repositories"][0].as_object().unwrap().len(), 5);
    assert_eq!(public.repositories[0].language.as_deref(), Some("Python"));
    assert!(
        f.remote
            .queries
            .lock()
            .unwrap()
            .iter()
            .any(|q| q.contains("per_page=20") && q.contains("page=2"))
    );
    for (_, headers) in f.remote.requests.lock().unwrap().iter() {
        assert_eq!(headers["authorization"], "Bearer code-private-github");
    }
    reply(
        &f,
        "/github/repos/team/web-app/branches",
        200,
        json!([{"name":"feature/login","commit":{"sha":SHA,"secret":"code-private-extra"}}]),
    );
    let (status, page) = get(&f.app, BRANCHES).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["branches"], json!(["feature/login"]));
    assert!(page["nextPage"].is_null());
    assert!(f.state.vms.lock().unwrap().is_empty());
}
#[tokio::test]
async fn code_catalog_rejects_nonoperators_bad_queries_and_identity_changes_before_listing() {
    let f = fixture().await;
    for uri in [
        format!("{REPOS}&page=0"),
        format!("{REPOS}&page=1001"),
        format!("{REPOS}&url=http://evil.invalid"),
        format!("{BRANCHES}&branch=main"),
        "/api/code/github/branches?account=octocat&owner=../../etc&repository=web-app".into(),
    ] {
        assert_eq!(get(&f.app, &uri).await.0, StatusCode::BAD_REQUEST);
    }
    assert!(f.remote.requests.lock().unwrap().is_empty());
    assert_eq!(
        get(&router(&f.state, false), REPOS).await.0,
        StatusCode::UNAUTHORIZED
    );
    reply(&f, "/github/user", 200, json!({"login":"other"}));
    assert_eq!(
        get(&f.app, REPOS).await.1["error"]["code"],
        "code_github_identity_changed"
    );
    assert_eq!(f.remote.requests.lock().unwrap().len(), 1);
    f.state.store.delete_pipe_auth(Provider::Github).unwrap();
    assert_eq!(
        get(&f.app, REPOS).await.1["error"]["code"],
        "code_github_missing"
    );
    assert_eq!(f.remote.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn code_catalog_bounds_remote_payloads_and_does_not_follow_redirects_or_echo_errors() {
    let f = fixture().await;
    for (status, body, code) in [
        (
            302,
            json!({"secret":"code-private"}),
            "code_github_redirected",
        ),
        (
            403,
            json!({"rateLimited":true,"secret":"code-private"}),
            "code_github_rate_limited",
        ),
        (
            200,
            json!((0..21).map(repo).collect::<Vec<_>>()),
            "code_github_invalid_response",
        ),
        (200, json!([{"name":"bad"}]), "code_github_invalid_response"),
        (
            200,
            json!({"big":"x".repeat(512*1024+1)}),
            "code_github_invalid_response",
        ),
    ] {
        reply(&f, "/github/user/repos", status, body);
        assert_eq!(get(&f.app, REPOS).await.1["error"]["code"], code);
    }
    reply(
        &f,
        "/github/repos/team/web-app/branches",
        200,
        json!([{"name":"main~evil"}]),
    );
    assert_eq!(
        get(&f.app, BRANCHES).await.1["error"]["code"],
        "code_github_invalid_response"
    );
    assert!(
        !f.remote
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path.contains("credential-leak-target"))
    );
}
#[tokio::test]
async fn code_catalog_discards_results_after_credentials_are_rotated_or_removed_by_another_connection()
 {
    let f = fixture().await;
    reply(&f, "/github/user/repos", 200, json!([repo(0)]));
    for remove in [false, true] {
        *f.remote.pause.lock().unwrap() = Some("/github/user/repos".into());
        let app = f.app.clone();
        let task = tokio::spawn(async move { get(&app, REPOS).await });
        f.remote.entered.notified().await;
        let other = Store::open(&f.dir.path().join("state.db")).unwrap();
        if remove {
            other.delete_pipe_auth(Provider::Github).unwrap();
        } else {
            other
                .put_pipe_auth(&SavedAuth::Github {
                    username: "octocat".into(),
                    token: "code-private-rotated".into(),
                })
                .unwrap();
        }
        f.remote.release.notify_one();
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "code_github_account_changed");
        assert!(body.get("repositories").is_none());
    }
}
