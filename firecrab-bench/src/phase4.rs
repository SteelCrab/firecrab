use std::time::Duration;

use reqwest::blocking::Client;

use crate::BenchmarkResult;

/// Publishes one completed result to `POST /api/benchmarks`.
pub fn publish_result(base: &str, result: &BenchmarkResult) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("failed to build benchmark publisher: {error}"))?;
    let response = client
        .post(format!("{}/api/benchmarks", base.trim_end_matches('/')))
        .json(result)
        .send()
        .map_err(|error| format!("failed to publish benchmark result: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "benchmark API returned HTTP {}: {}",
            response.status().as_u16(),
            response.text().unwrap_or_default()
        ))
    }
}

#[cfg(test)]
mod tests {
    use mockito::Server;

    use super::*;

    #[test]
    fn publisher_posts_the_common_schema() {
        let mut server = Server::new();
        let endpoint = server
            .mock("POST", "/api/benchmarks")
            .match_header("content-type", "application/json")
            .with_status(201)
            .create();
        let result = BenchmarkResult::from_counts("vm_boot", 1, 1, &[], Vec::new());
        publish_result(&server.url(), &result).unwrap();
        endpoint.assert();
    }

    #[test]
    fn publisher_preserves_api_failure() {
        let mut server = Server::new();
        let endpoint = server
            .mock("POST", "/api/benchmarks")
            .with_status(400)
            .with_body("invalid")
            .create();
        let result = BenchmarkResult::from_counts("vm_boot", 1, 1, &[], Vec::new());
        let error = publish_result(&server.url(), &result).unwrap_err();
        assert!(error.contains("HTTP 400"));
        endpoint.assert();
    }
}
