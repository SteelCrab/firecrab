use super::*;
use crate::templates::TemplateRegistry;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use firecrab_stack::{Binding, ConfigureGitSource, ResourceKind};
use serde_json::json;
use std::sync::{Arc, Mutex};

const YAML: &str = "apiVersion: firecrab.dev/v1alpha1\nkind: Stack\nname: demo\nmicroNetworks:\n  app:\n    name: existing\n    subnetCidr: 172.31.20.0/24\n    internetEnabled: true\n";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "cccccccccccccccccccccccccccccccccccccccc";
const TREE_A: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TREE_B: &str = "dddddddddddddddddddddddddddddddddddddddd";

struct Remote {
    first: Vec<u8>,
    second: Vec<u8>,
    head_b: bool,
    move_ref: bool,
    mode: &'static str,
    truncated: bool,
    tamper: bool,
    code: Option<StatusCode>,
    redirect: Option<String>,
    oversize: bool,
    seen: Vec<(String, Option<String>)>,
}

impl Default for Remote {
    fn default() -> Self {
        Self {
            first: YAML.as_bytes().into(),
            second: YAML
                .replace("internetEnabled: true", "internetEnabled: false")
                .into_bytes(),
            head_b: false,
            move_ref: false,
            mode: "100644",
            truncated: false,
            tamper: false,
            code: None,
            redirect: None,
            oversize: false,
            seen: vec![],
        }
    }
}

fn blob_id(bytes: &[u8]) -> String {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    hash.update(format!("blob {}\0", bytes.len()).as_bytes());
    hash.update(bytes);
    hash.finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn remote(
    State(remote): State<Arc<Mutex<Remote>>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let mut remote = remote.lock().unwrap();
    let path = uri.path();
    remote.seen.push((
        path.into(),
        headers
            .get("authorization")
            .map(|h| h.to_str().unwrap().into()),
    ));
    if let Some(code) = remote.code {
        return (code, "provider-echoed-private-secret").into_response();
    }
    if let Some(url) = &remote.redirect {
        return (
            StatusCode::FOUND,
            [("location", url.as_str())],
            "private-secret",
        )
            .into_response();
    }
    if remote.oversize {
        return "x".repeat(1024).into_response();
    }
    if path.ends_with("/commits/main") {
        let head = if remote.head_b { COMMIT_B } else { COMMIT_A };
        if remote.move_ref {
            remote.head_b = true;
        }
        return head.into_response();
    }
    if path.contains("/git/commits/") {
        let commit = path.rsplit('/').next().unwrap();
        return axum::Json(
            json!({"sha":commit,"tree":{"sha":if commit == COMMIT_A { TREE_A } else { TREE_B }}}),
        )
        .into_response();
    }
    if path.contains("/git/trees/") {
        let tree = path.rsplit('/').next().unwrap();
        let bytes = if tree == TREE_A {
            &remote.first
        } else {
            &remote.second
        };
        return axum::Json(json!({"sha":tree,"truncated":remote.truncated,"tree":[{"path":"stack.yaml","sha":blob_id(bytes),"mode":remote.mode,"type":if remote.mode == "160000" {"commit"} else {"blob"}}]})).into_response();
    }
    if path.contains("/git/blobs/") {
        let sha = path.rsplit('/').next().unwrap();
        let bytes = if sha == blob_id(&remote.first) {
            &remote.first
        } else {
            &remote.second
        };
        return axum::Json(json!({"sha":sha,"size":bytes.len(),"encoding":"base64","content":if remote.tamper {STANDARD.encode("x".repeat(bytes.len()))} else {STANDARD.encode(bytes)}})).into_response();
    }
    (StatusCode::NOT_FOUND, "private-secret").into_response()
}

async fn github(remote_state: Arc<Mutex<Remote>>) -> (Github, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/{*path}", get(remote))
        .with_state(remote_state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        Github {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            origin: format!("http://{address}"),
        },
        task,
    )
}

async fn fixture(root: &std::path::Path) -> (AppState, GitSourceStatus) {
    let templates = TemplateRegistry::from_specs(root, std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.join("state.db"))
        .await
        .unwrap();
    state.store.register_stack("demo").unwrap();
    let request: ConfigureGitSource = serde_json::from_value(json!({"expectedGeneration":0,"source":{"repository":"https://github.com/example/infra","ref":"main","manifestPath":"stack.yaml","paused":true}})).unwrap();
    let source = state
        .store
        .configure_stack_source("demo", &request, 0)
        .unwrap();
    (state, source)
}

async fn checked(state: &AppState, github: &Github) -> GitCheck {
    let (source, _) = state
        .store
        .begin_stack_check("demo", 1, now(), false)
        .unwrap()
        .unwrap();
    let mut check = source.latest.clone().unwrap();
    let result = inspect(state, &source, &mut check, github).await;
    complete(state, source.stack_id, check, result).await;
    state.store.stack_source("demo").unwrap().latest.unwrap()
}

#[tokio::test]
async fn exact_commit_tree_and_blob_remain_pinned_when_ref_moves_during_read() {
    let root = tempfile::tempdir().unwrap();
    let (state, _) = fixture(root.path()).await;
    let remote = Arc::new(Mutex::new(Remote {
        move_ref: true,
        ..Default::default()
    }));
    let (github, task) = github(remote.clone()).await;
    let first = checked(&state, &github).await;
    assert_eq!(first.commit.as_deref(), Some(COMMIT_A));
    assert_eq!(first.state, GitCheckState::Drifted);
    assert_eq!(
        first.blob.as_deref(),
        Some(blob_id(YAML.as_bytes()).as_str())
    );
    assert_eq!(first.plan.unwrap().changes[0].action, Action::Create);
    let second = checked(&state, &github).await;
    assert_eq!(second.commit.as_deref(), Some(COMMIT_B));
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
    let seen = &remote.lock().unwrap().seen;
    assert_eq!(seen.len(), 8);
    assert!(seen.iter().all(|(p, _)| !p.contains("contents")));
    assert!(seen[1].0.ends_with(COMMIT_A));
    assert!(seen[5].0.ends_with(COMMIT_B));
    task.abort();
}

#[tokio::test]
async fn unchanged_commit_detects_manual_drift_and_git_revert_without_changing_resources() {
    let root = tempfile::tempdir().unwrap();
    let (state, _) = fixture(root.path()).await;
    let remote = Arc::new(Mutex::new(Remote::default()));
    let (github, task) = github(remote.clone()).await;
    let mut network: firecrab_api_types::MicroNetworkResponse = serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"name":"existing","subnetCidr":"172.31.20.0/24","gateway":"172.31.20.1","internetEnabled":true})).unwrap();
    state.store.insert_micro_network(&network).unwrap();
    state
        .store
        .bind_stack_resource(
            "demo",
            &Binding {
                kind: ResourceKind::Network,
                key: "app".into(),
                resource_id: network.id,
            },
        )
        .unwrap();
    let first = checked(&state, &github).await;
    assert_eq!(first.state, GitCheckState::InSync);
    network.internet_enabled = false;
    state
        .store
        .set_micro_network_internet(network.id, false)
        .unwrap();
    let drift = checked(&state, &github).await;
    assert_eq!(drift.commit, first.commit);
    assert_eq!(drift.state, GitCheckState::Drifted);
    assert_eq!(
        drift.plan.unwrap().changes[0].fields,
        vec!["internetEnabled"]
    );
    assert!(
        !state
            .store
            .micro_network(network.id)
            .unwrap()
            .unwrap()
            .internet_enabled
    );
    remote.lock().unwrap().head_b = true;
    assert_eq!(checked(&state, &github).await.state, GitCheckState::InSync);
    remote.lock().unwrap().head_b = false;
    assert_eq!(checked(&state, &github).await.state, GitCheckState::Drifted);
    assert_eq!(state.store.stack_check_history("demo").unwrap().len(), 4);
    state.store.delete_micro_network(network.id).unwrap();
    let blocked = checked(&state, &github).await;
    assert_eq!(blocked.state, GitCheckState::Blocked);
    assert_eq!(blocked.plan.unwrap().changes[0].action, Action::Blocked);
    task.abort();
}

#[tokio::test]
async fn failed_sources_never_become_empty_stacks_or_expose_remote_errors() {
    for case in [
        "denied",
        "missing",
        "redirect",
        "oversized",
        "symlink",
        "submodule",
        "truncated",
        "invalid_yaml",
        "wrong_name",
        "hash",
        "missing_path",
        "utf8",
    ] {
        let root = tempfile::tempdir().unwrap();
        let (state, _) = fixture(root.path()).await;
        let mut spec = Remote::default();
        match case {
            "denied" => spec.code = Some(StatusCode::FORBIDDEN),
            "missing" => spec.code = Some(StatusCode::NOT_FOUND),
            "redirect" => spec.redirect = Some("http://127.0.0.1:1/private-secret".into()),
            "oversized" => spec.oversize = true,
            "symlink" => spec.mode = "120000",
            "submodule" => spec.mode = "160000",
            "truncated" => spec.truncated = true,
            "invalid_yaml" => spec.first = b"private-secret: [".to_vec(),
            "wrong_name" => spec.first = YAML.replace("name: demo", "name: other").into_bytes(),
            "hash" => spec.tamper = true,
            "missing_path" => {}
            "utf8" => spec.first = vec![0xff],
            _ => unreachable!(),
        }
        if case == "missing_path" {
            let mut source = state.store.stack_source("demo").unwrap();
            source.source.manifest_path = "absent.yaml".into();
            state
                .store
                .configure_stack_source(
                    "demo",
                    &ConfigureGitSource {
                        expected_generation: 1,
                        source: source.source,
                    },
                    0,
                )
                .unwrap();
            // Keep the fixture helper's generation fixed; use the generic path below instead.
        }
        let remote = Arc::new(Mutex::new(spec));
        let (github, task) = github(remote.clone()).await;
        let source = state.store.stack_source("demo").unwrap();
        let (source, _) = state
            .store
            .begin_stack_check("demo", source.generation, now(), false)
            .unwrap()
            .unwrap();
        let mut check = source.latest.clone().unwrap();
        let result = inspect(&state, &source, &mut check, &github).await;
        assert!(result.is_err(), "{case}");
        complete(&state, source.stack_id, check, result).await;
        let check = state.store.stack_source("demo").unwrap().latest.unwrap();
        assert_eq!(check.state, GitCheckState::Failed, "{case}");
        assert!(check.plan.is_none(), "{case}");
        assert!(
            !serde_json::to_string(&check)
                .unwrap()
                .contains("private-secret")
        );
        assert!(state.store.list_micro_networks().unwrap().is_empty());
        assert!(
            state
                .store
                .stack("demo")
                .unwrap()
                .unwrap()
                .bindings
                .is_empty()
        );
        if case == "redirect" {
            assert_eq!(remote.lock().unwrap().seen.len(), 1);
        }
        task.abort();
    }
}

#[tokio::test]
async fn private_source_uses_only_referenced_auth_and_keeps_the_token_out_of_status() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut source) = fixture(root.path()).await;
    source.source.credential_ref = Some(GitCredential::Github);
    state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: 1,
                source: source.source,
            },
            0,
        )
        .unwrap();
    state
        .store
        .put_pipe_auth(&SavedAuth::Github {
            username: "example".into(),
            token: "private-secret".into(),
        })
        .unwrap();
    let remote = Arc::new(Mutex::new(Remote::default()));
    let (github, task) = github(remote.clone()).await;
    let (source, _) = state
        .store
        .begin_stack_check("demo", 2, now(), false)
        .unwrap()
        .unwrap();
    let mut check = source.latest.clone().unwrap();
    let result = inspect(&state, &source, &mut check, &github).await;
    assert!(result.is_ok());
    complete(&state, source.stack_id, check, result).await;
    assert!(
        remote
            .lock()
            .unwrap()
            .seen
            .iter()
            .all(|(_, auth)| auth.as_deref() == Some("Bearer private-secret"))
    );
    let status = state.store.stack_source("demo").unwrap();
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("private-secret")
    );
    state
        .store
        .delete_pipe_auth(crate::pipe_auth::Provider::Github)
        .unwrap();
    let before = remote.lock().unwrap().seen.len();
    let (source, _) = state
        .store
        .begin_stack_check("demo", 2, now(), false)
        .unwrap()
        .unwrap();
    let mut check = source.latest.clone().unwrap();
    assert_eq!(
        inspect(&state, &source, &mut check, &github).await,
        Err("credential_unavailable")
    );
    assert_eq!(remote.lock().unwrap().seen.len(), before);
    task.abort();
}

#[tokio::test]
async fn periodic_checks_respect_pause_resume_and_durable_failure_backoff() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut source) = fixture(root.path()).await;
    source.source.paused = false;
    source.source.credential_ref = Some(GitCredential::Github);
    state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: 1,
                source: source.source,
            },
            now(),
        )
        .unwrap();
    let poller = spawn_reconciler(state.clone());
    async fn wait_failed(state: &AppState) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if state
                    .store
                    .stack_source("demo")
                    .unwrap()
                    .latest
                    .is_some_and(|c| c.state == GitCheckState::Failed)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    wait_failed(&state).await;
    let mut source = state.store.stack_source("demo").unwrap();
    assert_eq!(
        source.latest.as_ref().unwrap().error.as_deref(),
        Some("credential_unavailable")
    );
    source.source.paused = true;
    state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: 2,
                source: source.source,
            },
            now(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(state.store.stack_source("demo").unwrap().latest.is_none());
    assert_eq!(state.store.stack_check_history("demo").unwrap().len(), 1);
    let mut source = state.store.stack_source("demo").unwrap();
    source.source.paused = false;
    state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: 3,
                source: source.source,
            },
            now(),
        )
        .unwrap();
    wait_failed(&state).await;
    assert_eq!(state.store.stack_check_history("demo").unwrap().len(), 2);
    assert!(state.store.stack_source("demo").unwrap().next_check_at > now());
    poller.abort();
}

#[tokio::test]
#[ignore = "requires outbound GitHub API access; reads the public Firecrab example without mutating resources"]
async fn live_github_source_resolves_and_verifies_the_public_stack_manifest() {
    let root = tempfile::tempdir().unwrap();
    let (state, mut source) = fixture(root.path()).await;
    source.source.repository = "https://github.com/SteelCrab/firecrab".into();
    source.source.git_ref = "165116284db56f91d0c4a57fbc7fad1a6434c2a9".into();
    source.source.manifest_path = "examples/stacks/basic.yaml".into();
    let source = state
        .store
        .configure_stack_source(
            "demo",
            &ConfigureGitSource {
                expected_generation: 1,
                source: source.source,
            },
            0,
        )
        .unwrap();
    let (source, _) = state
        .store
        .begin_stack_check("demo", source.generation, now(), false)
        .unwrap()
        .unwrap();
    let mut check = source.latest.clone().unwrap();
    let result = inspect(&state, &source, &mut check, &Github::new().unwrap()).await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        check.commit.as_deref(),
        Some(source.source.git_ref.as_str())
    );
    assert!(check.blob.is_some());
    assert_eq!(check.state, GitCheckState::Drifted);
    assert_eq!(check.plan.as_ref().unwrap().changes.len(), 3);
    println!(
        "commit={} blob={} desiredHash={}",
        check.commit.as_deref().unwrap(),
        check.blob.as_deref().unwrap(),
        check.desired_hash.as_deref().unwrap()
    );
    complete(&state, source.stack_id, check, result).await;
    assert!(state.store.load_all().unwrap().is_empty());
    assert!(state.store.list_micro_networks().unwrap().is_empty());
    assert!(state.store.list_micro_services().unwrap().is_empty());
    assert!(
        state
            .store
            .stack("demo")
            .unwrap()
            .unwrap()
            .bindings
            .is_empty()
    );
}
