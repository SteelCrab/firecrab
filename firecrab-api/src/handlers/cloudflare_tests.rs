use super::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

async fn provider(responses: Vec<(u16, Value)>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for (index, (status, value)) in responses.into_iter().enumerate() {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert!(line.contains(&format!("/zones?per_page=50&page={}", index + 1)));
            loop {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            let body = value.to_string();
            socket.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    (base, task)
}

fn zone(name: &str, status: &str) -> Value {
    json!({"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":name,"status":status,"account":{"id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"Example"}})
}

#[tokio::test]
async fn lists_active_domains_across_pages_without_returning_the_token() {
    let (base,task)=provider(vec![(200,json!({"success":true,"result":[zone("z.example","active"),zone("pending.example","pending")],"result_info":{"total_pages":2}})),
        (200,json!({"success":true,"result":[zone("a.example","active")],"result_info":{"total_pages":2}}))]).await;
    let result = read_zones("private-token", &base).await.unwrap();
    assert_eq!(
        result
            .iter()
            .map(|zone| zone.name.as_str())
            .collect::<Vec<_>>(),
        vec!["a.example", "z.example"]
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private-token")
    );
    task.await.unwrap();
}

#[tokio::test]
async fn provider_errors_do_not_leak_their_body_and_explain_permissions() {
    let (base, task) = provider(vec![(
        403,
        json!({"errors":[{"message":"private-provider-body"}]}),
    )])
    .await;
    let error = read_zones("private-token", &base).await.unwrap_err();
    assert!(error.contains("Zone / Zone / Read"));
    assert!(!error.contains("private-provider-body"));
    task.await.unwrap();
}

#[tokio::test]
async fn rejects_provider_limits_invalid_payloads_and_excessively_broad_tokens() {
    for (status, payload, message) in [
        (429, json!({"secret":"provider-secret"}), "rate limit"),
        (503, json!({"secret":"provider-secret"}), "try again later"),
        (
            200,
            json!({"success":false,"secret":"provider-secret"}),
            "permissions",
        ),
        (
            200,
            json!({"success":true,"result":{}}),
            "Invalid Cloudflare domain response",
        ),
        (
            200,
            json!({"success":true,"result":[],"result_info":{"total_pages":11}}),
            "at most 500",
        ),
    ] {
        let (base, task) = provider(vec![(status, payload)]).await;
        let error = read_zones("private-token", &base).await.unwrap_err();
        assert!(error.contains(message), "{error}");
        assert!(!error.contains("provider-secret"));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn no_active_domain_explains_the_required_domain_setup() {
    let (base, task) = provider(vec![(
        200,
        json!({"success":true,"result":[zone("pending.example","pending")]}),
    )])
    .await;
    assert!(
        read_zones("token", &base)
            .await
            .unwrap_err()
            .contains("nameservers")
    );
    task.await.unwrap();
}

#[tokio::test]
async fn malformed_tokens_fail_before_cloudflare_or_helper_calls() {
    let request =
        serde_json::from_value(json!({"apiToken":"invalid-command --token secret"})).unwrap();
    assert!(
        zones(Extension(RequestId(uuid::Uuid::new_v4())), Json(request))
            .await
            .is_err()
    );
    let request=serde_json::from_value(json!({"apiToken":"invalid","zoneId":"invalid","hostname":"app.example.com","config":{"routes":[],"tunnel":null}})).unwrap();
    assert!(
        connect(Extension(RequestId(uuid::Uuid::new_v4())), Json(request))
            .await
            .is_err()
    );
}
