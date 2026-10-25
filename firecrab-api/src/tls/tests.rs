use super::*;
use axum::{extract::ConnectInfo, routing::get};

#[test]
fn plaintext_is_loopback_only() {
    assert!(
        configuration("127.0.0.1:5523".parse().unwrap(), None)
            .unwrap()
            .is_none()
    );
    assert!(
        configuration("[::1]:5523".parse().unwrap(), None)
            .unwrap()
            .is_none()
    );
    for address in [
        "0.0.0.0:5523",
        "192.0.2.1:5523",
        "[::]:5523",
        "[::ffff:127.0.0.1]:5523",
    ] {
        assert!(configuration(address.parse().unwrap(), None).is_err());
    }
}

async fn start(paths: TlsPaths) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = configuration(address, Some(paths)).unwrap();
    let app =
        Router::new().route(
            "/identity",
            get(
                |ConnectInfo(peer): ConnectInfo<AuthenticatedHost>| async move {
                    peer.host_id.to_string()
                },
            ),
        );
    let app = app.route(
        "/ws",
        get(
            |ConnectInfo(peer): ConnectInfo<AuthenticatedHost>,
             ws: axum::extract::ws::WebSocketUpgrade| async move {
                ws.on_upgrade(move |mut socket| async move {
                    socket
                        .send(axum::extract::ws::Message::Text(
                            peer.host_id.to_string().into(),
                        ))
                        .await
                        .unwrap();
                })
            },
        ),
    );
    let task = tokio::spawn(async move {
        serve(listener, app, config).await.unwrap();
    });
    (format!("https://{address}/identity"), task)
}

#[tokio::test]
async fn authenticates_ca_peer_and_ignores_claimed_http_identity() {
    let directory = tempfile::tempdir().unwrap();
    let ca = directory.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let server =
        firecrab_pki::issue_host_cert(&ca, Uuid::new_v4(), vec!["127.0.0.1".into()], 1).unwrap();
    let id = Uuid::new_v4();
    let client = firecrab_pki::issue_host_cert(&ca, id, vec![], 1).unwrap();
    let (url, task) = start(server).await;
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(client.client_config().unwrap())
        .build()
        .unwrap();
    let response = client
        .get(url)
        .header("x-firecrab-host-id", Uuid::new_v4().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), id.to_string());
    task.abort();
}

#[tokio::test]
async fn rejects_missing_client_certificate_and_foreign_ca() {
    let directory = tempfile::tempdir().unwrap();
    let ca = directory.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let server =
        firecrab_pki::issue_host_cert(&ca, Uuid::new_v4(), vec!["127.0.0.1".into()], 1).unwrap();
    let root =
        reqwest::Certificate::from_pem(&firecrab_pki::read_private(&server.ca).unwrap()).unwrap();
    let (url, task) = start(server).await;
    let no_cert = reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(root.clone())
        .build()
        .unwrap();
    assert!(no_cert.get(&url).send().await.is_err());
    let foreign_ca = directory.path().join("foreign");
    firecrab_pki::init_ca(&foreign_ca).unwrap();
    let foreign = firecrab_pki::issue_host_cert(&foreign_ca, Uuid::new_v4(), vec![], 1).unwrap();
    let mut pem = firecrab_pki::read_private(&foreign.cert).unwrap();
    pem.extend(firecrab_pki::read_private(&foreign.key).unwrap());
    let foreign = reqwest::Client::builder()
        .tls_built_in_root_certs(false)
        .add_root_certificate(root)
        .identity(reqwest::Identity::from_pem(&pem).unwrap())
        .build()
        .unwrap();
    assert!(foreign.get(&url).send().await.is_err());
    task.abort();
}

#[tokio::test]
async fn rejects_expired_future_or_invalid_host_identity_during_handshake() {
    use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, Issuer, KeyPair, SanType};
    use time::{Duration, OffsetDateTime};
    let directory = tempfile::tempdir().unwrap();
    let ca = directory.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let server =
        firecrab_pki::issue_host_cert(&ca, Uuid::new_v4(), vec!["127.0.0.1".into()], 1).unwrap();
    let root =
        reqwest::Certificate::from_pem(&firecrab_pki::read_private(&server.ca).unwrap()).unwrap();
    let ca_key = KeyPair::from_pem(
        &String::from_utf8(firecrab_pki::read_private(&ca.join("ca.key")).unwrap()).unwrap(),
    )
    .unwrap();
    let ca_pem =
        String::from_utf8(firecrab_pki::read_private(&ca.join("ca.crt")).unwrap()).unwrap();
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).unwrap();
    let (url, task) = start(server).await;
    for case in [
        "expired",
        "future",
        "bad-cn",
        "missing-san",
        "different-san",
        "wrong-usage",
    ] {
        let id = Uuid::new_v4();
        let mut params = CertificateParams::default();
        params.distinguished_name.push(
            DnType::CommonName,
            if case == "bad-cn" {
                "not-a-uuid".into()
            } else {
                id.to_string()
            },
        );
        if case != "missing-san" {
            let san_id = if case == "different-san" {
                Uuid::new_v4()
            } else {
                id
            };
            params.subject_alt_names.push(SanType::URI(
                format!("urn:firecrab:host:{san_id}").try_into().unwrap(),
            ));
        }
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(2);
        params.not_after = now + Duration::days(1);
        if case == "expired" {
            params.not_after = now - Duration::days(1);
        }
        if case == "future" {
            params.not_before = now + Duration::hours(1);
        }
        params.extended_key_usages = vec![if case == "wrong-usage" {
            ExtendedKeyUsagePurpose::ServerAuth
        } else {
            ExtendedKeyUsagePurpose::ClientAuth
        }];
        let key = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &issuer).unwrap();
        let identity = reqwest::Identity::from_pem(
            format!("{}{}", cert.pem(), key.serialize_pem()).as_bytes(),
        )
        .unwrap();
        let client = reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(root.clone())
            .identity(identity)
            .build()
            .unwrap();
        assert!(
            client.get(&url).send().await.is_err(),
            "accepted {case} certificate"
        );
    }
    task.abort();
}

#[tokio::test]
async fn a_stalled_handshake_does_not_block_an_authenticated_peer() {
    let directory = tempfile::tempdir().unwrap();
    let ca = directory.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let paths =
        firecrab_pki::issue_host_cert(&ca, Uuid::new_v4(), vec!["127.0.0.1".into()], 1).unwrap();
    let (url, task) = start(paths.clone()).await;
    let address = url
        .trim_start_matches("https://")
        .trim_end_matches("/identity");
    let _stalled = TcpStream::connect(address).await.unwrap();
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(paths.client_config().unwrap())
        .build()
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), client.get(&url).send())
        .await
        .unwrap()
        .unwrap();
    assert!(response.status().is_success());
    task.abort();
}

#[tokio::test]
async fn websocket_upgrade_preserves_the_authenticated_connection_identity() {
    use firecrab_pki::rustls::pki_types::ServerName;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = tempfile::tempdir().unwrap();
    let ca = directory.path().join("ca");
    firecrab_pki::init_ca(&ca).unwrap();
    let paths =
        firecrab_pki::issue_host_cert(&ca, Uuid::new_v4(), vec!["127.0.0.1".into()], 1).unwrap();
    let expected =
        firecrab_pki::host_id(&firecrab_pki::certificates(&paths.cert).unwrap()[0]).unwrap();
    let client = tokio_rustls::TlsConnector::from(Arc::new(paths.client_config().unwrap()));
    let (url, task) = start(paths).await;
    let address = url
        .trim_start_matches("https://")
        .trim_end_matches("/identity");
    let socket = TcpStream::connect(address).await.unwrap();
    let mut stream = client
        .connect(
            ServerName::IpAddress("127.0.0.1".parse::<std::net::IpAddr>().unwrap().into()),
            socket,
        )
        .await
        .unwrap();
    stream.write_all(b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").await.unwrap();
    let mut headers = Vec::new();
    let mut byte = [0];
    while !headers.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        headers.push(byte[0]);
    }
    assert!(headers.starts_with(b"HTTP/1.1 101"));
    let mut frame = [0; 38];
    stream.read_exact(&mut frame).await.unwrap();
    assert_eq!(&frame[..2], &[0x81, 36]);
    assert_eq!(&frame[2..], expected.to_string().as_bytes());
    task.abort();
}

#[tokio::test]
async fn default_loopback_still_serves_plain_http() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = configuration(address, None).unwrap();
    let app = Router::new().route("/ping", get(|| async { "ok" }));
    let task = tokio::spawn(serve(listener, app, config));
    assert_eq!(
        reqwest::get(format!("http://{address}/ping"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "ok"
    );
    task.abort();
}

#[tokio::test]
async fn serving_plaintext_on_a_remote_listener_is_rejected() {
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let error = serve(listener, Router::new(), None).await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
