use super::*;
use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
pub(crate) struct Registry {
    base: String,
    pub(crate) manifest: Vec<u8>,
    blobs: HashMap<String, Vec<u8>>,
    token_requests: Arc<AtomicUsize>,
    bearer: bool,
}
impl Registry {
    fn authorized(&self, headers: &HeaderMap) -> bool {
        let expected = if self.bearer {
            "Bearer scoped-token"
        } else {
            "Basic QVdTOnNlY3JldA=="
        };
        headers
            .get("authorization")
            .is_some_and(|header| header == expected)
    }
    fn challenge(&self) -> Response {
        let challenge = if self.bearer {
            format!(
                "Bearer realm=\"{}/token\",service=\"fixture\",scope=\"repository:team/app:pull\"",
                self.base
            )
        } else {
            "Basic realm=\"ECR\"".into()
        };
        (StatusCode::UNAUTHORIZED, [("www-authenticate", challenge)]).into_response()
    }
    pub(crate) fn credential(&self) -> Option<RegistryCredential> {
        Some(RegistryCredential {
            registry: self.base.trim_start_matches("http://").into(),
            username: "AWS".into(),
            secret: "secret".into(),
        })
    }
    pub(crate) fn reference(&self) -> ImageReference {
        ImageReference::parse(&format!(
            "{}/team/app:v1",
            self.base.trim_start_matches("http://")
        ))
        .unwrap()
    }
}

pub(crate) async fn fixture(bearer: bool) -> (Registry, tokio::task::JoinHandle<()>) {
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": if Architecture::HOST == Architecture::X86_64 { "amd64" } else { "arm64" },
        "os":"linux", "rootfs":{"type":"layers","diff_ids":[]}
    }))
    .unwrap();
    let layer = b"original compressed layer bytes".to_vec();
    let config_digest = Sha256Digest::of_bytes(&config);
    let layer_digest = Sha256Digest::of_bytes(&layer);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
        "annotations":{"keep":"original annotations"},
        "config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":config_digest.to_string(),"size":config.len()},
        "layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":layer_digest.to_string(),"size":layer.len()}]
    })).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = Registry {
        base: format!("http://{}", listener.local_addr().unwrap()),
        manifest,
        blobs: [
            (config_digest.to_string(), config),
            (layer_digest.to_string(), layer),
        ]
        .into(),
        token_requests: Arc::default(),
        bearer,
    };
    let app = Router::new()
        .route("/token", get(|State(state): State<Registry>, Query(query): Query<HashMap<String, String>>, headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Basic QVdTOnNlY3JldA==");
            assert_eq!(query["scope"], "repository:team/app:pull");
            state.token_requests.fetch_add(1, Ordering::SeqCst);
            axum::Json(serde_json::json!({"token":"scoped-token"}))
        }))
        .route("/v2/team/app/tags/list", get(|State(state): State<Registry>, Query(query): Query<HashMap<String,String>>, headers: HeaderMap| async move {
            if !state.authorized(&headers) { return state.challenge(); }
            let mut response = axum::Json(serde_json::json!({"tags": if query.contains_key("last") { vec!["v2"] } else { vec!["v1"] }})).into_response();
            if !query.contains_key("last") { response.headers_mut().insert("link", HeaderValue::from_static("</v2/team/app/tags/list?last=v1&n=100>; rel=\"next\"")); }
            response
        }))
        .route("/v2/team/app/manifests/{selector}", get(|State(state): State<Registry>, headers: HeaderMap| async move {
            if !state.authorized(&headers) { return state.challenge(); }
            ([("content-type", "application/vnd.oci.image.manifest.v1+json")], state.manifest).into_response()
        }))
        .route("/v2/team/app/blobs/{digest}", get(|State(state): State<Registry>, AxumPath(digest): AxumPath<String>, headers: HeaderMap| async move {
            if !state.authorized(&headers) { return state.challenge(); }
            state.blobs.get(&digest).unwrap().clone().into_response()
        })).with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (state, task)
}

#[tokio::test]
async fn lists_tags_and_pulls_with_ecr_basic_and_ghcr_bearer_logins() {
    let root = tempfile::tempdir().unwrap();
    let (basic, basic_task) = fixture(false).await;
    let page = tags(&basic.reference(), None, basic.credential())
        .await
        .unwrap();
    assert_eq!(page.tags, ["v1"]);
    assert_eq!(page.next_last.as_deref(), Some("v1"));
    let page = tags(
        &basic.reference(),
        page.next_last.as_deref(),
        basic.credential(),
    )
    .await
    .unwrap();
    assert_eq!(page.tags, ["v2"]);
    assert_eq!(page.next_last, None);
    let pulled = pull(root.path(), &basic.reference(), basic.credential())
        .await
        .unwrap();
    assert_eq!(
        pulled.digest,
        Sha256Digest::of_bytes(&basic.manifest).to_string()
    );
    assert_eq!(
        pulled.size_bytes,
        basic
            .blobs
            .values()
            .map(|blob| blob.len() as u64)
            .sum::<u64>()
    );
    basic_task.abort();

    let (bearer, bearer_task) = fixture(true).await;
    let again = pull(root.path(), &bearer.reference(), bearer.credential())
        .await
        .unwrap();
    assert_eq!(again.id, pulled.id);
    // The fixture's token endpoint checks the Basic login and pull scope on every exchange.
    assert!(bearer.token_requests.load(Ordering::SeqCst) > 0);
    let listed = images(root.path()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].reference, again.reference);
    bearer_task.abort();
}

#[tokio::test]
async fn credentials_never_follow_a_different_registry_or_token_service() {
    let (source, task) = fixture(true).await;
    let reference = source.reference();
    let mut credential = source.credential().unwrap();
    credential.registry = "ghcr.io".into();
    let session = RegistrySession::new(&reference, true, Some(credential)).unwrap();
    assert!(session.basic.is_none());
    let session = RegistrySession::new(&reference, true, source.credential()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let challenge =
        "Bearer realm=\"https://untrusted.invalid/token\",scope=\"repository:team/app:pull\""
            .to_string();
    let app = Router::new().route(
        "/",
        get(move || {
            let challenge = challenge.clone();
            async move { (StatusCode::UNAUTHORIZED, [("www-authenticate", challenge)]) }
        }),
    );
    let address = listener.local_addr().unwrap();
    let other_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let response = reqwest::get(format!("http://{address}/")).await.unwrap();
    assert!(
        session
            .authorize(&response, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("untrusted")
    );
    task.abort();
    other_task.abort();
}

#[test]
fn refuses_single_manifests_with_a_wrong_or_missing_platform() {
    assert!(
        validate_platform(
            br#"{"os":"linux","architecture":"amd64"}"#,
            Architecture::X86_64
        )
        .is_ok()
    );
    assert!(
        validate_platform(
            br#"{"os":"linux","architecture":"arm64"}"#,
            Architecture::Aarch64
        )
        .is_ok()
    );
    assert!(
        validate_platform(
            br#"{"os":"linux","architecture":"arm64"}"#,
            Architecture::X86_64
        )
        .is_err()
    );
    assert!(
        validate_platform(
            br#"{"os":"windows","architecture":"amd64"}"#,
            Architecture::X86_64
        )
        .is_err()
    );
    assert!(validate_platform(br#"{}"#, Architecture::X86_64).is_err());
}
