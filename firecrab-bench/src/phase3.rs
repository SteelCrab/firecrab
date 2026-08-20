use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::resources::{HostResourceSampler, count_firecracker_processes};
use crate::runner::lifecycle_once;
use crate::{BenchmarkResult, VmApi, VmSpec};

/// Repeats VM lifecycles until the duration or optional iteration bound.
pub fn run_soak<A: VmApi>(
    api: &A,
    spec: &VmSpec,
    duration: Duration,
    max_iterations: Option<u32>,
) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    let mut sequence = 1;
    while started.elapsed() < duration && max_iterations.is_none_or(|limit| sequence <= limit) {
        match lifecycle_once(api, spec, "soak", sequence) {
            Ok(elapsed) => samples.push(elapsed),
            Err(error) => failures.push(format!("iteration {sequence}: {error}")),
        }
        sequence += 1;
    }
    let attempted = samples.len() as u32 + failures.len() as u32;
    BenchmarkResult::new("soak", attempted, &samples, failures)
        .with_metric("duration_seconds", started.elapsed().as_secs_f64())
        .with_metric("completed_iterations", samples.len() as f64)
        .with_host_resources(resources.finish())
}

/// Repeats VM lifecycles and reports positive Linux resource deltas as leaks.
pub fn run_leak_check<A: VmApi>(api: &A, spec: &VmSpec, iterations: u32) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let before = LeakSnapshot::capture();
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for sequence in 1..=iterations {
        match lifecycle_once(api, spec, "leak", sequence) {
            Ok(elapsed) => samples.push(elapsed),
            Err(error) => failures.push(format!("iteration {sequence}: {error}")),
        }
    }
    let after = LeakSnapshot::capture();
    let firecracker_delta =
        positive_delta(after.firecracker_processes, before.firecracker_processes);
    let tap_delta = positive_delta(after.tap_devices, before.tap_devices);
    let namespace_delta = positive_delta(after.network_namespaces, before.network_namespaces);
    let fd_delta = positive_delta(after.firecrab_api_fds, before.firecrab_api_fds);
    let leak_count = firecracker_delta + tap_delta + namespace_delta + fd_delta;
    if leak_count > 0 {
        failures.push(format!("detected {leak_count} leaked host resources"));
    }
    BenchmarkResult::from_counts(
        "resource_leak",
        iterations,
        samples.len() as u32,
        &samples,
        failures,
    )
    .with_metric("firecracker_process_delta", f64::from(firecracker_delta))
    .with_metric("tap_device_delta", f64::from(tap_delta))
    .with_metric("network_namespace_delta", f64::from(namespace_delta))
    .with_metric("firecrab_api_fd_delta", f64::from(fd_delta))
    .with_metric(
        "host_memory_growth_mib",
        positive_delta(after.memory_used_mib, before.memory_used_mib) as f64,
    )
    .with_metric("resource_leak_count", f64::from(leak_count))
    .with_host_resources(resources.finish())
}

/// Compares one metric in two result files and fails above the threshold.
pub fn run_regression_files(
    baseline_path: &Path,
    current_path: &Path,
    metric: &str,
    threshold_percent: f64,
) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let loaded = (|| {
        let baseline = read_result(baseline_path)?;
        let current = read_result(current_path)?;
        compare_results(&baseline, &current, metric, threshold_percent)
    })();
    match loaded {
        Ok(result) => result.with_host_resources(resources.finish()),
        Err(error) => {
            BenchmarkResult::from_counts("performance_regression", 1, 0, &[], vec![error])
                .with_host_resources(resources.finish())
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LeakSnapshot {
    firecracker_processes: u32,
    tap_devices: u32,
    network_namespaces: u32,
    firecrab_api_fds: u32,
    memory_used_mib: u32,
}

impl LeakSnapshot {
    fn capture() -> Self {
        Self {
            firecracker_processes: count_firecracker_processes(),
            tap_devices: count_entries("/sys/class/net", |name| name.starts_with("fct")),
            network_namespaces: count_entries("/var/run/netns", |_| true),
            firecrab_api_fds: count_process_fds("firecrab-api"),
            memory_used_mib: host_memory_used_mib(),
        }
    }
}

fn compare_results(
    baseline: &BenchmarkResult,
    current: &BenchmarkResult,
    metric: &str,
    threshold_percent: f64,
) -> Result<BenchmarkResult, String> {
    let baseline_value = metric_value(baseline, metric)
        .ok_or_else(|| format!("baseline has no metric named {metric}"))?;
    let current_value = metric_value(current, metric)
        .ok_or_else(|| format!("current result has no metric named {metric}"))?;
    if baseline_value == 0.0 {
        return Err("baseline metric must not be zero".to_owned());
    }
    let higher_is_better = is_higher_better(metric);
    let change_percent = (current_value - baseline_value) * 100.0 / baseline_value;
    let regression_percent = if higher_is_better {
        -change_percent
    } else {
        change_percent
    };
    let failures = if regression_percent > threshold_percent {
        vec![format!(
            "{metric} regressed by {regression_percent:.2}% (threshold {threshold_percent:.2}%)"
        )]
    } else {
        Vec::new()
    };
    Ok(BenchmarkResult::from_counts(
        "performance_regression",
        1,
        u32::from(failures.is_empty()),
        &[],
        failures,
    )
    .with_metric("baseline_value", baseline_value)
    .with_metric("current_value", current_value)
    .with_metric("change_percent", change_percent)
    .with_metric("regression_percent", regression_percent)
    .with_metric("threshold_percent", threshold_percent))
}

fn read_result(path: &Path) -> Result<BenchmarkResult, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn metric_value(result: &BenchmarkResult, metric: &str) -> Option<f64> {
    match metric {
        "average_ms" => result.latency.as_ref().map(|latency| latency.average_ms),
        "p50_ms" => result.latency.as_ref().map(|latency| latency.p50_ms as f64),
        "p95_ms" => result.latency.as_ref().map(|latency| latency.p95_ms as f64),
        "p99_ms" => result.latency.as_ref().map(|latency| latency.p99_ms as f64),
        "failure_rate" => Some(result.failure_rate),
        other => result.metrics.get(other).copied(),
    }
}

fn is_higher_better(metric: &str) -> bool {
    metric.contains("throughput")
        || metric.contains("per_second")
        || metric == "iops"
        || metric == "max_stable_microvms"
}

fn positive_delta(after: u32, before: u32) -> u32 {
    after.saturating_sub(before)
}

fn count_entries(path: &str, predicate: impl Fn(&str) -> bool) -> u32 {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| predicate(&entry.file_name().to_string_lossy()))
        .count() as u32
}

fn count_process_fds(process_name: &str) -> u32 {
    let Ok(entries) = fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let comm = fs::read_to_string(entry.path().join("comm")).ok()?;
            (comm.trim() == process_name).then_some(entry.path().join("fd"))
        })
        .map(|path| count_entries(path.to_string_lossy().as_ref(), |_| true))
        .sum()
}

fn host_memory_used_mib() -> u32 {
    let Ok(contents) = fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    let value = |key: &str| {
        contents.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == key)
                .then(|| fields.next()?.parse::<u64>().ok())
                .flatten()
        })
    };
    let Some(total) = value("MemTotal:") else {
        return 0;
    };
    let Some(available) = value("MemAvailable:") else {
        return 0;
    };
    total.saturating_sub(available).div_ceil(1024) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use firecrab_api_types::VmState;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::{ApiError, LatencySummary};

    struct FakeApi(Mutex<HashMap<Uuid, VmState>>);

    impl FakeApi {
        fn new() -> Self {
            Self(Mutex::new(HashMap::new()))
        }
    }

    impl VmApi for FakeApi {
        fn create(&self, _spec: &VmSpec, _name: &str) -> Result<Uuid, ApiError> {
            let id = Uuid::new_v4();
            self.0.lock().unwrap().insert(id, VmState::Created);
            Ok(id)
        }

        fn start(&self, id: Uuid) -> Result<(), ApiError> {
            self.0.lock().unwrap().insert(id, VmState::Running);
            Ok(())
        }

        fn state(&self, id: Uuid) -> Result<VmState, ApiError> {
            self.0
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .ok_or(ApiError::NotFound(id))
        }

        fn stop(&self, id: Uuid) -> Result<(), ApiError> {
            self.0.lock().unwrap().insert(id, VmState::Stopped);
            Ok(())
        }

        fn delete(&self, id: Uuid) -> Result<(), ApiError> {
            self.0.lock().unwrap().remove(&id);
            Ok(())
        }
    }

    fn spec() -> VmSpec {
        VmSpec {
            template: "ubuntu".to_owned(),
            micro_network_id: Uuid::new_v4(),
            ram: 512,
            cpu: 1,
            disk_gb: 8,
        }
    }

    #[test]
    fn soak_honors_the_iteration_bound() {
        let result = run_soak(&FakeApi::new(), &spec(), Duration::from_secs(60), Some(2));
        assert_eq!(result.successful_count, 2);
        assert_eq!(result.metrics["completed_iterations"], 2.0);
    }

    #[test]
    fn leak_check_reports_no_fake_resource_leaks() {
        let result = run_leak_check(&FakeApi::new(), &spec(), 2);
        assert_eq!(result.successful_count, 2);
        assert_eq!(result.metrics["resource_leak_count"], 0.0);
    }

    #[test]
    fn regression_detects_slower_latency() {
        let baseline = result_with_p95(100);
        let current = result_with_p95(125);
        let result = compare_results(&baseline, &current, "p95_ms", 10.0).unwrap();
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.metrics["regression_percent"], 25.0);
    }

    #[test]
    fn regression_files_accept_improved_throughput() {
        let directory = tempdir().unwrap();
        let baseline_path = directory.path().join("baseline.json");
        let current_path = directory.path().join("current.json");
        let baseline = BenchmarkResult::from_counts("network", 1, 1, &[], Vec::new())
            .with_metric("throughput_mbps", 100.0);
        let current = BenchmarkResult::from_counts("network", 1, 1, &[], Vec::new())
            .with_metric("throughput_mbps", 110.0);
        fs::write(&baseline_path, serde_json::to_string(&baseline).unwrap()).unwrap();
        fs::write(&current_path, serde_json::to_string(&current).unwrap()).unwrap();
        let result = run_regression_files(&baseline_path, &current_path, "throughput_mbps", 5.0);
        assert_eq!(result.failed_count, 0);
    }

    fn result_with_p95(p95_ms: u64) -> BenchmarkResult {
        BenchmarkResult::from_counts("vm_boot", 1, 1, &[], Vec::new()).with_latency(
            LatencySummary {
                average_ms: p95_ms as f64,
                p50_ms: p95_ms,
                p95_ms,
                p99_ms: p95_ms,
                minimum_ms: p95_ms,
                maximum_ms: p95_ms,
            },
        )
    }
}
