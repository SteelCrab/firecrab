//! Bounded retries for idempotent registry GETs before response headers arrive.

use std::time::Duration;

use super::{ResolveError, format_error_chain};

pub(super) async fn get(
    mut request: reqwest::RequestBuilder,
    url: &str,
) -> Result<reqwest::Response, ResolveError> {
    let mut attempt = 1;
    loop {
        let retry = request.try_clone();
        let result = request.send().await;
        if attempt < 3
            && let Err(error) = &result
            && (error.is_connect() || error.is_timeout())
            && let Some(next) = retry
        {
            // Brief resolver outages can outlast an immediate retry. Leave
            // three, then six seconds for recovery, still capped at three GETs.
            let delay = Duration::from_secs(3 * attempt);
            tracing::warn!(
                attempt,
                delay_secs = delay.as_secs(),
                "retrying registry GET after a connection failure"
            );
            tokio::time::sleep(delay).await;
            request = next;
            attempt += 1;
            continue;
        }
        // HTTP/authentication errors and failures while reading a response
        // body keep their existing handling; no partial blob is replayed here.
        return result.map_err(|error| {
            ResolveError::Transport(format!("GET {url}: {}", format_error_chain(&error)))
        });
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get as route_get;
    use reqwest::dns::{Addrs, Name, Resolve, Resolving};

    use super::*;

    struct FlakyDns {
        calls: Arc<AtomicUsize>,
        failures: usize,
        address: SocketAddr,
    }

    impl Resolve for FlakyDns {
        fn resolve(&self, _: Name) -> Resolving {
            let fails = self.calls.fetch_add(1, Ordering::SeqCst) < self.failures;
            let address = self.address;
            Box::pin(async move {
                if fails {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "Temporary failure in name resolution",
                    )
                    .into())
                } else {
                    Ok(Box::new(std::iter::once(address)) as Addrs)
                }
            })
        }
    }

    async fn server(
        status: StatusCode,
        slow_first: bool,
    ) -> (SocketAddr, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let received = calls.clone();
        let app = Router::new().route(
            "/",
            route_get(move |headers: HeaderMap| {
                let first = received.fetch_add(1, Ordering::SeqCst) == 0;
                async move {
                    assert_eq!(
                        headers["accept"],
                        "application/vnd.oci.image.manifest.v1+json"
                    );
                    assert_eq!(headers["authorization"], "Bearer fixture-token");
                    if slow_first && first {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    (status, "fixture")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (address, calls, task)
    }

    fn request(
        address: SocketAddr,
        failures: usize,
        timeout: Duration,
    ) -> (reqwest::RequestBuilder, String, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .dns_resolver(Arc::new(FlakyDns {
                calls: calls.clone(),
                failures,
                address,
            }))
            .build()
            .unwrap();
        let url = format!("http://registry.invalid:{}/", address.port());
        let request = client
            .get(&url)
            .header("accept", "application/vnd.oci.image.manifest.v1+json")
            .bearer_auth("fixture-token")
            .timeout(timeout);
        (request, url, calls)
    }

    #[tokio::test]
    async fn a_temporary_dns_failure_recovers_without_losing_request_headers() {
        let (address, received, task) = server(StatusCode::OK, false).await;
        let (request, url, dns_calls) = request(address, 2, Duration::from_secs(5));
        let response = get(request, &url).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "fixture");
        assert_eq!(dns_calls.load(Ordering::SeqCst), 3);
        assert_eq!(received.load(Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test]
    async fn a_persistent_dns_failure_stops_after_three_attempts_and_keeps_its_cause() {
        let (request, url, calls) = request(
            "127.0.0.1:1".parse().unwrap(),
            usize::MAX,
            Duration::from_secs(5),
        );
        let error = get(request, &url).await.unwrap_err().to_string();
        assert!(
            error.contains("Temporary failure in name resolution"),
            "{error}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn http_authentication_and_server_failures_are_not_retried() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::SERVICE_UNAVAILABLE] {
            let (address, received, task) = server(status, false).await;
            let (request, url, _) = request(address, 0, Duration::from_secs(5));
            assert_eq!(get(request, &url).await.unwrap().status(), status);
            assert_eq!(received.load(Ordering::SeqCst), 1);
            task.abort();
        }
    }

    #[tokio::test]
    async fn a_stall_before_response_headers_retries_the_get() {
        let (address, received, task) = server(StatusCode::OK, true).await;
        let (request, url, _) = request(address, 0, Duration::from_millis(100));
        assert_eq!(get(request, &url).await.unwrap().status(), StatusCode::OK);
        assert_eq!(received.load(Ordering::SeqCst), 2);
        task.abort();
    }
}
