use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::resources::HostResourceSampler;
use crate::{BenchmarkResult, LatencySummary};

/// Parameters for one iperf3 network benchmark.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Host name or IP address of an iperf3 server.
    pub target: String,
    /// Test duration in seconds.
    pub duration_seconds: u32,
    /// Number of parallel iperf3 streams.
    pub parallel: u32,
    /// Requests server-to-client traffic instead of client-to-server traffic.
    pub reverse: bool,
    /// Uses UDP instead of TCP.
    pub udp: bool,
    /// UDP target bitrate accepted by iperf3, such as `1G`.
    pub bitrate: String,
}

/// fio access patterns supported by the storage benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    /// Sequential reads.
    SequentialRead,
    /// Sequential writes.
    SequentialWrite,
    /// Random reads.
    RandomRead,
    /// Random writes.
    RandomWrite,
}

impl StorageMode {
    fn fio_name(self) -> &'static str {
        match self {
            Self::SequentialRead => "read",
            Self::SequentialWrite => "write",
            Self::RandomRead => "randread",
            Self::RandomWrite => "randwrite",
        }
    }

    fn result_key(self) -> &'static str {
        match self {
            Self::SequentialRead | Self::RandomRead => "read",
            Self::SequentialWrite | Self::RandomWrite => "write",
        }
    }
}

/// Parameters for one fio storage benchmark.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Directory where fio creates its uniquely named temporary file.
    pub directory: PathBuf,
    /// I/O access pattern.
    pub mode: StorageMode,
    /// Block size accepted by fio, such as `4k` or `1m`.
    pub block_size: String,
    /// Temporary test file size in MiB.
    pub size_mib: u32,
    /// Number of fio jobs.
    pub jobs: u32,
}

/// Runs concurrent read-only requests against one Firecrab API path.
pub fn run_api_load(base: &str, path: &str, requests: u32, concurrency: u32) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let suite_started = Instant::now();
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client construction");
    let next = AtomicU32::new(0);
    let samples = Mutex::new(Vec::new());
    let failures = Mutex::new(Vec::new());
    let base = base.trim_end_matches('/');
    thread::scope(|scope| {
        for _ in 0..concurrency {
            let client = client.clone();
            let next = &next;
            let samples = &samples;
            let failures = &failures;
            scope.spawn(move || {
                loop {
                    let sequence = next.fetch_add(1, Ordering::Relaxed);
                    if sequence >= requests {
                        break;
                    }
                    let started = Instant::now();
                    match client.get(format!("{base}{path}")).send() {
                        Ok(response) if response.status().is_success() => {
                            samples.lock().unwrap().push(started.elapsed());
                        }
                        Ok(response) => failures.lock().unwrap().push(format!(
                            "request {}: HTTP {}",
                            sequence + 1,
                            response.status().as_u16()
                        )),
                        Err(error) => failures
                            .lock()
                            .unwrap()
                            .push(format!("request {}: {error}", sequence + 1)),
                    }
                }
            });
        }
    });
    let elapsed = suite_started.elapsed();
    let samples = samples.into_inner().unwrap();
    let failures = failures.into_inner().unwrap();
    let rate = samples.len() as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
    BenchmarkResult::new("api_load", requests, &samples, failures)
        .with_metric("concurrency", f64::from(concurrency))
        .with_metric("requests_per_second", rate)
        .with_metric("total_time_ms", elapsed.as_millis() as f64)
        .with_host_resources(resources.finish())
}

/// Runs iperf3 and converts its JSON output into the common result schema.
pub fn run_network(config: &NetworkConfig) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let mut command = Command::new("iperf3");
    command.args([
        "-c",
        &config.target,
        "-t",
        &config.duration_seconds.to_string(),
        "-P",
        &config.parallel.to_string(),
        "-J",
    ]);
    if config.reverse {
        command.arg("-R");
    }
    if config.udp {
        command.args(["-u", "-b", &config.bitrate]);
    }
    let result = match command.output() {
        Ok(output) if output.status.success() => network_result(&output.stdout),
        Ok(output) => failed_result("network", String::from_utf8_lossy(&output.stderr).trim()),
        Err(error) => failed_result("network", &format!("failed to execute iperf3: {error}")),
    };
    result.with_host_resources(resources.finish())
}

/// Runs fio against a unique temporary file and converts its JSON output.
pub fn run_storage(config: &StorageConfig) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let filename = format!("firecrab-bench-{}.fio", Uuid::new_v4().simple());
    let output = Command::new("fio")
        .arg("--name=firecrab-bench")
        .arg(format!("--rw={}", config.mode.fio_name()))
        .arg(format!("--bs={}", config.block_size))
        .arg(format!("--size={}M", config.size_mib))
        .arg(format!("--numjobs={}", config.jobs))
        .arg(format!("--directory={}", config.directory.display()))
        .arg(format!("--filename={filename}"))
        .args([
            "--output-format=json",
            "--group_reporting=1",
            "--unlink=1",
            "--direct=1",
        ])
        .output();
    let result = match output {
        Ok(output) if output.status.success() => storage_result(&output.stdout, config.mode),
        Ok(output) => failed_result("storage", String::from_utf8_lossy(&output.stderr).trim()),
        Err(error) => failed_result("storage", &format!("failed to execute fio: {error}")),
    };
    result.with_host_resources(resources.finish())
}

fn network_result(output: &[u8]) -> BenchmarkResult {
    let Ok(value) = serde_json::from_slice::<Value>(output) else {
        return failed_result("network", "iperf3 returned invalid JSON");
    };
    let summary = value
        .pointer("/end/sum_received")
        .or_else(|| value.pointer("/end/sum"));
    let Some(summary) = summary else {
        return failed_result("network", "iperf3 JSON has no end summary");
    };
    let Some(bits_per_second) = summary.get("bits_per_second").and_then(Value::as_f64) else {
        return failed_result("network", "iperf3 JSON has no throughput");
    };
    let mut result = BenchmarkResult::from_counts("network", 1, 1, &[], Vec::new())
        .with_metric("throughput_mbps", bits_per_second / 1_000_000.0);
    for (json_key, metric_key) in [
        ("lost_percent", "packet_loss_percent"),
        ("jitter_ms", "jitter_ms"),
        ("retransmits", "retransmits"),
    ] {
        if let Some(value) = summary.get(json_key).and_then(Value::as_f64) {
            result = result.with_metric(metric_key, value);
        }
    }
    result
}

fn storage_result(output: &[u8], mode: StorageMode) -> BenchmarkResult {
    let Ok(value) = serde_json::from_slice::<Value>(output) else {
        return failed_result("storage", "fio returned invalid JSON");
    };
    let Some(io) = value.pointer(&format!("/jobs/0/{}", mode.result_key())) else {
        return failed_result("storage", "fio JSON has no job result");
    };
    let Some(iops) = io.get("iops").and_then(Value::as_f64) else {
        return failed_result("storage", "fio JSON has no IOPS value");
    };
    let Some(bandwidth) = io.get("bw_bytes").and_then(Value::as_f64) else {
        return failed_result("storage", "fio JSON has no bandwidth value");
    };
    let mut result = BenchmarkResult::from_counts("storage", 1, 1, &[], Vec::new())
        .with_metric("iops", iops)
        .with_metric("throughput_mb_per_second", bandwidth / 1_000_000.0);
    if let Some(latency) = fio_latency(io) {
        result = result.with_latency(latency);
    }
    result
}

fn fio_latency(io: &Value) -> Option<LatencySummary> {
    let latency = io.get("clat_ns")?;
    let percentile = latency.get("percentile")?;
    let milliseconds = |key: &str| {
        percentile
            .get(key)
            .and_then(Value::as_f64)
            .map(|value| (value / 1_000_000.0).round() as u64)
    };
    Some(LatencySummary {
        average_ms: latency.get("mean")?.as_f64()? / 1_000_000.0,
        p50_ms: milliseconds("50.000000")?,
        p95_ms: milliseconds("95.000000")?,
        p99_ms: milliseconds("99.000000")?,
        minimum_ms: latency.get("min")?.as_u64()? / 1_000_000,
        maximum_ms: latency.get("max")?.as_u64()? / 1_000_000,
    })
}

fn failed_result(test: &str, error: &str) -> BenchmarkResult {
    BenchmarkResult::from_counts(test, 1, 0, &[], vec![error.to_owned()])
}

#[cfg(test)]
mod tests {
    use mockito::Server;

    use super::*;

    #[test]
    fn api_load_collects_latency_and_throughput() {
        let mut server = Server::new();
        let requests = server
            .mock("GET", "/api/vms")
            .with_status(200)
            .expect(4)
            .create();
        let result = run_api_load(&server.url(), "/api/vms", 4, 2);
        assert_eq!(result.successful_count, 4);
        assert_eq!(result.failed_count, 0);
        assert!(result.metrics["requests_per_second"] > 0.0);
        requests.assert();
    }

    #[test]
    fn parses_tcp_network_result() {
        let output = br#"{"end":{"sum_received":{"bits_per_second":125000000.0,"retransmits":2}}}"#;
        let result = network_result(output);
        assert_eq!(result.metrics["throughput_mbps"], 125.0);
        assert_eq!(result.metrics["retransmits"], 2.0);
    }

    #[test]
    fn parses_storage_result_and_latency() {
        let output = br#"{"jobs":[{"read":{"iops":5000.0,"bw_bytes":20480000.0,"clat_ns":{"mean":1500000.0,"min":500000,"max":4000000,"percentile":{"50.000000":1000000.0,"95.000000":2000000.0,"99.000000":3000000.0}}}}]}"#;
        let result = storage_result(output, StorageMode::RandomRead);
        assert_eq!(result.metrics["iops"], 5000.0);
        assert_eq!(result.latency.unwrap().p99_ms, 3);
    }

    #[test]
    fn invalid_external_json_becomes_a_failure() {
        assert_eq!(network_result(b"bad json").failed_count, 1);
        assert_eq!(
            storage_result(b"{}", StorageMode::SequentialWrite).failed_count,
            1
        );
    }
}
