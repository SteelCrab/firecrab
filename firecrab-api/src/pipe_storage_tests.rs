use super::*;
use axum::{Router, response::Response as HttpResponse};
use http_body_util::BodyExt;

#[test]
fn listing_names_decode_spaces_and_literal_plus_without_double_decoding() {
    assert_eq!(decode_key("qa%2Fa+b%2Bc%2520").unwrap(), "qa/a b+c%20");
    assert_eq!(decode_key("qa%2Fa%20b%2Bc").unwrap(), "qa/a b+c");
    assert_eq!(decode_key("qa%2Fa%252Fb").unwrap(), "qa/a%2Fb");
}
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub(crate) fn keys() -> AwsCredentials {
    AwsCredentials {
        access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
        secret_access_key: "never-expose-test-secret".into(),
        session_token: "never-expose-session".into(),
        region: "ap-northeast-2".into(),
    }
}
pub(crate) fn input() -> StorageInput {
    serde_json::from_value(serde_json::json!({
        "name":"Test bucket", "bucket":"firecrab-test-bucket", "region":"ap-northeast-2", "prefix":"team/",
        "credentialRef":"connection", "credentials": {"accessKeyId":keys().access_key_id,"secretAccessKey":keys().secret_access_key,"sessionToken":keys().session_token}
    })).unwrap()
}
pub(crate) fn record() -> StorageRecord {
    input().record(Uuid::new_v4(), None).unwrap()
}
pub(crate) fn listing(contents: &str, continuation: &str) -> String {
    format!(
        "<ListBucketResult><EncodingType>url</EncodingType><Prefix>team%2F</Prefix>{contents}{continuation}</ListBucketResult>"
    )
}
pub(crate) fn reply(status: StatusCode, body: impl Into<String>) -> HttpResponse {
    HttpResponse::builder()
        .status(status)
        .body(Body::from(body.into()))
        .unwrap()
}
pub(crate) struct Captured {
    pub method: Method,
    pub uri: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
pub(crate) struct Fixture {
    pub origin: String,
    pub requests: tokio::sync::mpsc::UnboundedReceiver<Captured>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(crate) async fn fixture(replies: Vec<HttpResponse>) -> Fixture {
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let (sender, requests) = tokio::sync::mpsc::unbounded_channel();
    let router =
        Router::new().fallback(move |request: axum::extract::Request| {
            let replies = replies.clone();
            let sender = sender.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = body.collect().await.unwrap().to_bytes().to_vec();
                let _ = sender.send(Captured {
                    method: parts.method,
                    uri: parts.uri.to_string(),
                    headers: parts.headers,
                    body: bytes,
                });
                replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                    reply(StatusCode::INTERNAL_SERVER_ERROR, "unexpected request")
                })
            }
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        origin,
        requests,
        task,
    }
}

#[test]
fn inputs_reject_invalid_hosts_paths_and_unknown_fields_without_echoing_keys() {
    for (field, value) in [
        ("bucket", "evil.test/path"),
        ("bucket", "BUCKET"),
        ("bucket", "a--x-s3"),
        ("bucket", "ab"),
        ("region", "region.evil.test"),
        ("prefix", "team/../other/"),
        ("prefix", "team/./other/"),
    ] {
        let mut value_json = serde_json::to_value(&record().spec).unwrap();
        value_json[field] = value.into();
        value_json["credentialRef"] = "pipeAuthAws".into();
        let input: StorageInput = serde_json::from_value(value_json).unwrap();
        assert!(
            input.record(Uuid::new_v4(), None).is_err(),
            "{field}={value}"
        );
    }
    let mut data = serde_json::to_value(&record().spec).unwrap();
    data["endpoint"] = "https://evil.test".into();
    assert!(serde_json::from_value::<StorageInput>(data).is_err());
    let mut input = input();
    input.spec.prefix = "team".into();
    assert_eq!(
        input.record(Uuid::nil(), None).unwrap().spec.prefix,
        "team/"
    );
}

#[test]
fn persistence_reopens_and_resolves_credentials_without_ecr_or_public_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("state.db");
    let saved = record();
    let store = Store::open(&file).unwrap();
    store.save_pipe_storage(&saved).unwrap();
    assert_eq!(
        saved.signing_credentials(&store).unwrap().secret_access_key,
        keys().secret_access_key
    );
    drop(store);
    let store = Store::open(&file).unwrap();
    let reopened = store.pipe_storage(saved.id).unwrap().unwrap();
    assert_eq!(reopened.spec, saved.spec);
    let mut replacement = input();
    replacement.credentials = None;
    let updated = replacement.record(saved.id, Some(&reopened)).unwrap();
    assert_eq!(
        updated.credentials.unwrap().session_token,
        keys().session_token
    );
    store
        .put_pipe_auth(&SavedAuth::Aws {
            account_id: "123456789012".into(),
            credentials: keys(),
        })
        .unwrap();
    let mut shared = reopened.clone();
    shared.credentials = None;
    shared.spec.credential_ref = StorageCredentialRef::PipeAuthAws;
    shared.spec.region = "us-east-1".into();
    assert_eq!(
        shared.signing_credentials(&store).unwrap().region,
        "us-east-1"
    );
    store
        .delete_pipe_auth(crate::pipe_auth::Provider::Aws)
        .unwrap();
    assert!(shared.signing_credentials(&store).is_err());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(store.delete_pipe_storage(saved.id).unwrap());
    assert!(store.pipe_storage(saved.id).unwrap().is_none());
}

#[tokio::test]
async fn listings_sign_session_credentials_and_roundtrip_unicode_keys_and_cursors() {
    let first = listing(
        "<Contents><Key>team%2F%ED%95%9C%EA%B8%80+%2B%25%3F.txt</Key><Size>123</Size><LastModified>2026-10-23T00:00:00Z</LastModified></Contents><CommonPrefixes><Prefix>team%2Fdir%2F</Prefix></CommonPrefixes>",
        "<IsTruncated>true</IsTruncated><NextContinuationToken>opaque+/=&amp;next</NextContinuationToken>",
    );
    let mut fixture = fixture(vec![
        reply(StatusCode::OK, first),
        reply(
            StatusCode::OK,
            listing("", "<IsTruncated>false</IsTruncated>"),
        ),
    ])
    .await;
    let client = S3Client::new(record(), keys())
        .unwrap()
        .with_test_origin(&fixture.origin);
    let page = client.list("team/", None, 2).await.unwrap();
    assert_eq!(page.objects[0].key, "team/한글 +%?.txt");
    assert_eq!(page.prefixes, ["team/dir/"]);
    let token = page.next_cursor.as_deref().unwrap();
    assert_eq!(token, "opaque+/=&next");
    assert!(
        client
            .list("team/", Some(token), 2)
            .await
            .unwrap()
            .next_cursor
            .is_none()
    );
    let first = fixture.requests.recv().await.unwrap();
    assert_eq!(first.method, Method::GET);
    assert_eq!(first.headers["x-amz-security-token"], keys().session_token);
    assert_eq!(first.headers["x-amz-content-sha256"], "UNSIGNED-PAYLOAD");
    assert!(
        first.headers["authorization"]
            .to_str()
            .unwrap()
            .contains("/ap-northeast-2/s3/aws4_request")
    );
    assert!(first.uri.contains("prefix=team%2F"));
    assert!(
        fixture
            .requests
            .recv()
            .await
            .unwrap()
            .uri
            .contains("continuation-token=opaque%2B%2F%3D%26next")
    );
}

#[tokio::test]
async fn streaming_transfers_preserve_bytes_and_protect_existing_objects() {
    let data = "payload%\n".repeat(200_000);
    let mut fixture = fixture(vec![
        reply(StatusCode::OK, ""),
        reply(StatusCode::OK, data.clone()),
        reply(StatusCode::NO_CONTENT, ""),
        reply(
            StatusCode::PRECONDITION_FAILED,
            "<Error><Message>never-expose-test-secret</Message></Error>",
        ),
    ])
    .await;
    let client = S3Client::new(record(), keys())
        .unwrap()
        .with_test_origin(&fixture.origin);
    let key = "team/a +?%/한글.bin";
    client
        .upload(key, data.len() as u64, false, Body::from(data.clone()))
        .await
        .unwrap();
    let captured = fixture.requests.recv().await.unwrap();
    assert_eq!(captured.body, data.as_bytes());
    assert_eq!(captured.headers["if-none-match"], "*");
    assert!(
        captured
            .uri
            .starts_with("/team/a%20%2B%3F%25/%ED%95%9C%EA%B8%80.bin")
    );
    assert_eq!(
        client.download(key).await.unwrap().text().await.unwrap(),
        data
    );
    client.delete(key).await.unwrap();
    let error = client
        .upload(key, 0, false, Body::empty())
        .await
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(!format!("{error:?}").contains("never-expose"));
}

#[tokio::test]
async fn verification_is_read_only_and_upstream_errors_do_not_echo_bodies() {
    for (code, status, expected) in [
        ("AccessDenied", StatusCode::FORBIDDEN, StatusCode::FORBIDDEN),
        (
            "ExpiredToken",
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "SignatureDoesNotMatch",
            StatusCode::FORBIDDEN,
            StatusCode::UNAUTHORIZED,
        ),
        ("NoSuchBucket", StatusCode::NOT_FOUND, StatusCode::NOT_FOUND),
        (
            "AuthorizationHeaderMalformed",
            StatusCode::BAD_REQUEST,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let mut fixture = fixture(vec![reply(
            status,
            format!(
                "<Error><Code>{code}</Code><Message>never-expose-test-secret</Message></Error>"
            ),
        )])
        .await;
        let client = S3Client::new(record(), keys())
            .unwrap()
            .with_test_origin(&fixture.origin);
        let error = client.verify().await.err().unwrap();
        assert_eq!(error.status, expected, "{code}");
        assert!(!format!("{error:?}").contains("never-expose"));
        let request = fixture.requests.recv().await.unwrap();
        assert_eq!(request.method, Method::GET);
        assert!(request.uri.contains("max-keys=1"));
        assert!(request.body.is_empty());
    }
}

#[tokio::test]
async fn prefix_and_size_guards_run_before_network_and_foreign_results_are_rejected() {
    let mut fixture = fixture(vec![reply(StatusCode::OK,listing("<Contents><Key>other%2Fsecret</Key><Size>1</Size><LastModified>today</LastModified></Contents>",""))]).await;
    let client = S3Client::new(record(), keys())
        .unwrap()
        .with_test_origin(&fixture.origin);
    for key in [
        "team-other/key",
        "other/key",
        "team/../secret",
        "team/./secret",
        "",
    ] {
        assert!(client.delete(key).await.is_err(), "{key}");
    }
    assert!(
        client
            .upload("team/key", MAX_OBJECT_BYTES + 1, false, Body::empty())
            .await
            .is_err()
    );
    assert!(fixture.requests.try_recv().is_err());
    assert!(client.list("team/", None, 100).await.is_err());
    fixture.requests.recv().await.unwrap();
    assert!(fixture.requests.try_recv().is_err());
}

#[tokio::test]
async fn malformed_and_oversized_metadata_and_invalid_pagination_fail_explicitly() {
    for xml in [
        "not xml".to_string(),
        "x".repeat(MAX_XML_BYTES + 1),
        listing("", "<IsTruncated>true</IsTruncated>"),
        listing(
            "",
            "<NextContinuationToken></NextContinuationToken><IsTruncated>true</IsTruncated>",
        ),
    ] {
        let fixture = fixture(vec![reply(StatusCode::OK, xml)]).await;
        let client = S3Client::new(record(), keys())
            .unwrap()
            .with_test_origin(&fixture.origin);
        assert!(client.list("team/", None, 100).await.is_err());
    }
}

#[tokio::test]
async fn expired_tokens_with_the_correct_region_are_not_reported_as_region_mismatches() {
    let mut response = reply(
        StatusCode::BAD_REQUEST,
        "<Error><Code>ExpiredToken</Code></Error>",
    );
    response.headers_mut().insert(
        "x-amz-bucket-region",
        HeaderValue::from_static("ap-northeast-2"),
    );
    let fixture = fixture(vec![response]).await;
    let client = S3Client::new(record(), keys())
        .unwrap()
        .with_test_origin(&fixture.origin);
    let error = client.verify().await.err().unwrap();
    assert_eq!(error.code, "storage_credentials_expired");
}

#[tokio::test]
async fn listings_reject_inconsistent_scope_encoding_and_directory_metadata() {
    let contents = |key: &str| {
        format!(
            "<Contents><Key>{key}</Key><Size>1</Size><LastModified>today</LastModified></Contents>"
        )
    };
    let directory =
        |prefix: &str| format!("<CommonPrefixes><Prefix>{prefix}</Prefix></CommonPrefixes>");
    let subdirectory_listing = |entries: &str| {
        listing(entries, "").replace("<Prefix>team%2F</Prefix>", "<Prefix>team%2Fsub%2F</Prefix>")
    };
    for (name, prefix, limit, xml) in [
        (
            "unexpected encoding",
            "team/",
            100,
            listing("", "").replace(
                "<EncodingType>url</EncodingType>",
                "<EncodingType>plain</EncodingType>",
            ),
        ),
        ("wrong response prefix", "team/sub/", 100, listing("", "")),
        (
            "more entries than requested",
            "team/",
            1,
            listing(&(contents("team%2Fone") + &directory("team%2Ftwo%2F")), ""),
        ),
        (
            "invalid UTF-8 object key",
            "team/",
            100,
            listing(&contents("team%2F%FF"), ""),
        ),
        (
            "object outside the selected subdirectory",
            "team/sub/",
            100,
            subdirectory_listing(&contents("team%2Fother")),
        ),
        (
            "directory outside the selected subdirectory",
            "team/sub/",
            100,
            subdirectory_listing(&directory("team%2Fother%2F")),
        ),
        (
            "directory without a trailing slash",
            "team/",
            100,
            listing(&directory("team%2Fother"), ""),
        ),
        (
            "directory outside the connection",
            "team/",
            100,
            listing(&directory("other%2F"), ""),
        ),
    ] {
        let fixture = fixture(vec![reply(StatusCode::OK, xml)]).await;
        let client = S3Client::new(record(), keys())
            .unwrap()
            .with_test_origin(&fixture.origin);
        assert!(client.list(prefix, None, limit).await.is_err(), "{name}");
    }
}
