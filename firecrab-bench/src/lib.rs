//! Firecrab benchmark clients, runners, and normalized result types.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;

mod client;
mod resources;
mod runner;

pub use client::{ApiError, HttpVmApi, VmApi, VmSpec};
pub use resources::{HostResourceUsage, RunMetadata};
pub use runner::{BenchmarkError, run_boot, run_concurrent_creation, run_density, run_lifecycle};

/// Aggregated latency values in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LatencySummary {
    /// Arithmetic mean of all successful observations.
    pub average_ms: f64,
    /// 50th percentile, using the nearest-rank method.
    pub p50_ms: u64,
    /// 95th percentile, using the nearest-rank method.
    pub p95_ms: u64,
    /// 99th percentile, using the nearest-rank method.
    pub p99_ms: u64,
    /// Fastest successful observation.
    pub minimum_ms: u64,
    /// Slowest successful observation.
    pub maximum_ms: u64,
}

impl LatencySummary {
    /// Aggregates non-empty duration samples into a stable JSON-ready form.
    pub fn from_samples(samples: &[Duration]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut milliseconds = samples
            .iter()
            .map(|sample| sample.as_millis() as u64)
            .collect::<Vec<_>>();
        milliseconds.sort_unstable();
        let average_ms = milliseconds.iter().sum::<u64>() as f64 / milliseconds.len() as f64;
        Some(Self {
            average_ms,
            p50_ms: percentile(&milliseconds, 50),
            p95_ms: percentile(&milliseconds, 95),
            p99_ms: percentile(&milliseconds, 99),
            minimum_ms: milliseconds[0],
            maximum_ms: milliseconds[milliseconds.len() - 1],
        })
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (sorted.len() * percentile).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// The normalized result emitted by a benchmark command.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkResult {
    /// Version of this serialized result contract.
    pub schema_version: u8,
    /// Commit, branch, timestamp, host, and version metadata.
    pub run: RunMetadata,
    /// Benchmark case identifier, for example `vm_boot`.
    pub test: String,
    /// Number of requested benchmark operations.
    pub requested_count: u32,
    /// Number of operations actually attempted before an early stop.
    pub attempted_count: u32,
    /// Number of successful benchmark operations.
    pub successful_count: u32,
    /// Number of failed benchmark operations.
    pub failed_count: u32,
    /// Failed operations as a percentage of all requested operations.
    pub failure_rate: f64,
    /// Latency statistics for successful operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency: Option<LatencySummary>,
    /// Per-operation failure messages; omitted when every operation succeeds.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    /// Benchmark-specific numeric values such as VM/s or maximum density.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, f64>,
    /// Host CPU, memory, load, and Firecracker process measurements.
    pub host_resources: HostResourceUsage,
}

impl BenchmarkResult {
    /// Builds a result while preserving enough failure detail for CI logs.
    pub fn new(
        test: &str,
        requested_count: u32,
        samples: &[Duration],
        failures: Vec<String>,
    ) -> Self {
        let successful_count = samples.len() as u32;
        let failed_count = failures.len() as u32;
        let attempted_count = successful_count + failed_count;
        Self {
            schema_version: 2,
            run: RunMetadata::capture(),
            test: test.to_owned(),
            requested_count,
            attempted_count,
            successful_count,
            failed_count,
            failure_rate: f64::from(failed_count) * 100.0 / f64::from(attempted_count.max(1)),
            latency: LatencySummary::from_samples(samples),
            failures,
            metrics: BTreeMap::new(),
            host_resources: HostResourceUsage::default(),
        }
    }

    /// Adds one command-specific metric to the serialized result.
    pub fn with_metric(mut self, name: &str, value: f64) -> Self {
        self.metrics.insert(name.to_owned(), value);
        self
    }

    /// Attaches host resource measurements collected across the command.
    pub fn with_host_resources(mut self, resources: HostResourceUsage) -> Self {
        self.host_resources = resources;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_summary_uses_nearest_rank_percentiles() {
        let samples = [10, 20, 30, 40, 50].map(Duration::from_millis);
        assert_eq!(
            LatencySummary::from_samples(&samples),
            Some(LatencySummary {
                average_ms: 30.0,
                p50_ms: 30,
                p95_ms: 50,
                p99_ms: 50,
                minimum_ms: 10,
                maximum_ms: 50,
            })
        );
    }

    #[test]
    fn empty_samples_have_no_latency_summary() {
        assert_eq!(LatencySummary::from_samples(&[]), None);
    }

    #[test]
    fn result_calculates_failure_rate() {
        let result = BenchmarkResult::new(
            "vm_boot",
            4,
            &[Duration::from_millis(10), Duration::from_millis(20)],
            vec!["first failure".to_owned(), "second failure".to_owned()],
        );
        assert_eq!(result.successful_count, 2);
        assert_eq!(result.failed_count, 2);
        assert_eq!(result.attempted_count, 4);
        assert_eq!(result.failure_rate, 50.0);
    }

    #[test]
    fn result_accepts_command_specific_metrics() {
        let result = BenchmarkResult::new("vm_density", 10, &[], Vec::new())
            .with_metric("max_stable_microvms", 8.0);
        assert_eq!(result.metrics["max_stable_microvms"], 8.0);
    }

    #[test]
    fn result_uses_the_common_schema() {
        let result = BenchmarkResult::new("vm_boot", 1, &[Duration::from_millis(1)], Vec::new());
        assert_eq!(result.schema_version, 2);
        assert_ne!(result.run.run_id, uuid::Uuid::nil());
        assert_eq!(result.host_resources, HostResourceUsage::default());
    }
}
