use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

const ZONE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ACCOUNT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const RECORD: &str = "cccccccccccccccccccccccccccccccc";
const TUNNEL: &str = "11111111-1111-4111-8111-111111111111";
type Step = (&'static str, String, u16, Value);
type Calls = Arc<Mutex<Vec<Value>>>;

fn request() -> ConnectRequest {
    serde_json::from_value(json!({"apiToken":"scoped-cloudflare-token-01234567890123456789","zoneId":ZONE,"hostname":"app.example.com","config":{"routes":[{"host":"app.example.com","upstreams":["127.0.0.1:18081"]}],"tunnel":null}})).unwrap()
}

fn server(steps: Vec<Step>) -> (Cloudflare, std::thread::JoinHandle<()>, Calls) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = Cloudflare::new(&request()).unwrap();
    client.base = format!("http://{}/", listener.local_addr().unwrap());
    let calls: Calls = Arc::default();
    let captured = calls.clone();
    let thread = std::thread::spawn(move || {
        for (method, path, status, value) in steps {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with(&format!("{method} /{path} ")), "{line}");
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            captured
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
            let body = serde_json::to_vec(&json!({"success":status == 200,"result":value,"errors":[{"message":"provider-secret-must-not-leak"}]})).unwrap();
            write!(
                stream,
                "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (client, thread, calls)
}

fn preflight() -> Vec<Step> {
    vec![
        (
            "GET",
            format!("zones/{ZONE}"),
            200,
            json!({"id":ZONE,"name":"example.com","status":"active","account":{"id":ACCOUNT,"name":"Example"}}),
        ),
        (
            "GET",
            format!("zones/{ZONE}/dns_records?name=app.example.com"),
            200,
            json!([]),
        ),
    ]
}
fn creates() -> Vec<Step> {
    let mut steps = preflight();
    steps.extend([
        (
            "POST",
            format!("accounts/{ACCOUNT}/cfd_tunnel"),
            200,
            json!({"id":TUNNEL}),
        ),
        (
            "POST",
            format!("zones/{ZONE}/dns_records"),
            200,
            json!({"id":RECORD}),
        ),
    ]);
    steps
}
fn remove_tunnel() -> Vec<Step> {
    vec![
        (
            "DELETE",
            format!("accounts/{ACCOUNT}/cfd_tunnel/{TUNNEL}/connections"),
            200,
            json!({}),
        ),
        (
            "DELETE",
            format!("accounts/{ACCOUNT}/cfd_tunnel/{TUNNEL}"),
            200,
            json!({}),
        ),
    ]
}

#[test]
fn creates_local_tunnel_and_proxied_dns_then_verifies_connector() {
    let mut steps = creates();
    steps.push((
        "GET",
        format!("accounts/{ACCOUNT}/cfd_tunnel/{TUNNEL}/connections"),
        200,
        json!([{"id":"connection"}]),
    ));
    let (client, server, calls) = server(steps);
    client
        .provision(&request(), |config, credentials, account, tunnel| {
            config.validate_credentials(&credentials)?;
            assert_eq!(account, ACCOUNT);
            assert_eq!(tunnel.to_string(), TUNNEL);
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(credentials["TunnelSecret"].as_str().unwrap())
                    .unwrap()
                    .len(),
                32
            );
            client.wait_connected(account, tunnel)
        })
        .unwrap();
    server.join().unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(calls[2]["config_src"], "local");
    assert_eq!(calls[3]["content"], format!("{TUNNEL}.cfargotunnel.com"));
    assert_eq!(calls[3]["proxied"], true);
    assert!(
        !calls
            .iter()
            .any(|body| body.to_string().contains(request().api_token.value()))
    );
}

#[test]
fn existing_dns_is_never_overwritten_or_followed_by_local_apply() {
    let mut steps = preflight();
    steps[1].3 = json!([{"type":"A","content":"192.0.2.1"}]);
    let (client, server, _) = server(steps);
    let error = client
        .provision(&request(), |_, _, _, _| panic!("must not apply"))
        .unwrap_err();
    assert!(error.to_string().contains("never overwritten"));
    server.join().unwrap();
}

#[test]
fn foreign_domain_and_inactive_domain_fail_before_any_mutation() {
    for (name, status) in [("other.example", "active"), ("example.com", "pending")] {
        let steps = vec![(
            "GET",
            format!("zones/{ZONE}"),
            200,
            json!({"id":ZONE,"name":name,"status":status,"account":{"id":ACCOUNT}}),
        )];
        let (client, server, _) = server(steps);
        assert!(
            client
                .provision(&request(), |_, _, _, _| panic!("must not apply"))
                .is_err()
        );
        server.join().unwrap();
    }
}

#[test]
fn rejected_token_has_actionable_permissions_and_no_provider_secrets() {
    let (client, server, _) = server(vec![("GET", format!("zones/{ZONE}"), 403, json!({}))]);
    let error = client
        .provision(&request(), |_, _, _, _| panic!("must not apply"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("DNS Edit"));
    assert!(!error.contains("provider-secret"));
    assert!(!error.contains(request().api_token.value()));
    server.join().unwrap();
}

#[test]
fn provider_rate_limit_and_server_errors_are_sanitized_before_any_mutation() {
    for (status, message) in [(429, "rate limit"), (503, "HTTP 503")] {
        let (client, server, _) = server(vec![("GET", format!("zones/{ZONE}"), status, json!({}))]);
        let error = client
            .provision(&request(), |_, _, _, _| panic!("must not apply"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{error}");
        assert!(!error.contains("provider-secret"));
        server.join().unwrap();
    }
}

#[test]
fn local_apply_failure_removes_only_new_dns_and_tunnel() {
    let mut steps = creates();
    steps.push((
        "DELETE",
        format!("zones/{ZONE}/dns_records/{RECORD}"),
        200,
        json!({}),
    ));
    steps.extend(remove_tunnel());
    let (client, server, _) = server(steps);
    let error = client
        .provision(&request(), |_, _, _, _| Err("local apply rejected".into()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("local apply rejected"));
    assert!(error.contains("resources removed"));
    server.join().unwrap();
}

#[test]
fn dns_creation_failure_removes_the_new_tunnel() {
    let mut steps = creates();
    steps[3].2 = 403;
    steps.push((
        "GET",
        format!("zones/{ZONE}/dns_records?name=app.example.com"),
        200,
        json!([]),
    ));
    steps.extend(remove_tunnel());
    let (client, server, _) = server(steps);
    assert!(
        client
            .provision(&request(), |_, _, _, _| panic!("must not apply"))
            .unwrap_err()
            .to_string()
            .contains("resources removed")
    );
    server.join().unwrap();
}

#[test]
fn failed_compensation_reports_the_hostname_and_tunnel_for_recovery() {
    let mut steps = creates();
    steps.push((
        "DELETE",
        format!("zones/{ZONE}/dns_records/{RECORD}"),
        403,
        json!({}),
    ));
    steps.extend(remove_tunnel());
    let (client, server, _) = server(steps);
    let error = client
        .provision(&request(), |_, _, _, _| Err("local apply failed".into()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("cleanup was incomplete"));
    assert!(error.contains(TUNNEL));
    assert!(error.contains("app.example.com"));
    server.join().unwrap();
}
