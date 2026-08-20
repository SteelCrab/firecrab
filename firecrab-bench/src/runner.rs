//! Core MicroVM benchmark algorithms and lifecycle cleanup helpers.

use std::thread;
use std::time::{Duration, Instant};

use firecrab_api_types::VmState;
use thiserror::Error;
use uuid::Uuid;

use crate::resources::HostResourceSampler;
use crate::{ApiError, BenchmarkResult, VmApi, VmSpec};

/// Interval between VM state observations.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Maximum wait for one requested VM state transition.
const STATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Failure from one benchmark VM operation.
#[derive(Debug, Error)]
pub enum BenchmarkError {
    /// Firecrab API operation failed.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// VM did not reach its expected state before the deadline.
    #[error("VM {id} did not reach {expected} within the state timeout")]
    Timeout {
        /// VM being observed.
        id: Uuid,
        /// Expected state name.
        expected: &'static str,
    },
    /// VM entered the error state while booting.
    #[error("VM {0} entered error state during boot")]
    FailedBoot(Uuid),
    /// A concurrent worker panicked.
    #[error("concurrent benchmark worker panicked")]
    WorkerPanic,
}

/// Result of one create-and-boot attempt, including its cleanup handle.
struct Operation {
    /// Created VM identifier when creation reached the API.
    id: Option<Uuid>,
    /// Measured latency or operation failure.
    result: Result<Duration, BenchmarkError>,
}

/// Measures sequential create-request through running-state boot latency.
pub fn run_boot<A: VmApi>(api: &A, spec: &VmSpec, count: u32) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for sequence in 1..=count {
        let operation = boot_once(api, spec, "boot", sequence);
        record_operation(api, operation, sequence, &mut samples, &mut failures);
    }
    BenchmarkResult::new("vm_boot", count, &samples, failures)
        .with_host_resources(resources.finish())
}

/// Creates and boots one concurrent VM group.
pub fn run_concurrent_creation<A: VmApi>(
    api: &A,
    spec: &VmSpec,
    concurrency: u32,
) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let started = Instant::now();
    let operations = boot_group(api, spec, "create", 1, concurrency);
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for (index, operation) in operations.into_iter().enumerate() {
        record_operation(
            api,
            operation,
            index as u32 + 1,
            &mut samples,
            &mut failures,
        );
    }
    let elapsed = started.elapsed();
    let rate = samples.len() as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
    BenchmarkResult::new("concurrent_creation", concurrency, &samples, failures)
        .with_metric("total_creation_time_ms", elapsed.as_millis() as f64)
        .with_metric("vm_per_second", rate)
        .with_host_resources(resources.finish())
}

/// Adds VM batches until `max_vms` is reached or a batch becomes unstable.
pub fn run_density<A: VmApi>(
    api: &A,
    spec: &VmSpec,
    max_vms: u32,
    step: u32,
    settle_time: Duration,
) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let mut running = Vec::new();
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    let mut sequence = 1;
    while sequence <= max_vms {
        let batch = step.min(max_vms - sequence + 1);
        let operations = boot_group(api, spec, "density", sequence, batch);
        let mut batch_failed = false;
        for (offset, operation) in operations.into_iter().enumerate() {
            let current = sequence + offset as u32;
            match operation.result {
                Ok(elapsed) => {
                    samples.push(elapsed);
                    running.push(operation.id.expect("successful boot has VM id"));
                }
                Err(error) => {
                    batch_failed = true;
                    failures.push(format!("VM {current}: {error}"));
                    cleanup_if_created(api, operation.id);
                }
            }
        }
        thread::sleep(settle_time);
        for id in running.clone() {
            if !matches!(api.state(id), Ok(VmState::Running)) {
                batch_failed = true;
                failures.push(format!("VM {id} left running state during stability check"));
                running.retain(|running_id| *running_id != id);
                cleanup_if_created(api, Some(id));
            }
        }
        sequence += batch;
        if batch_failed {
            break;
        }
    }
    let max_stable = running.len() as f64;
    for id in running {
        cleanup_if_created(api, Some(id));
    }
    BenchmarkResult::new("vm_density", max_vms, &samples, failures)
        .with_metric("max_stable_microvms", max_stable)
        .with_host_resources(resources.finish())
}

/// Repeats create/start/stop/start/stop/delete and measures each full cycle.
pub fn run_lifecycle<A: VmApi>(api: &A, spec: &VmSpec, iterations: u32) -> BenchmarkResult {
    let resources = HostResourceSampler::start();
    let suite_started = Instant::now();
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for sequence in 1..=iterations {
        match lifecycle_once(api, spec, "lifecycle", sequence) {
            Ok(elapsed) => samples.push(elapsed),
            Err(error) => failures.push(format!("iteration {sequence}: {error}")),
        }
    }
    let elapsed = suite_started.elapsed();
    let rate = samples.len() as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
    BenchmarkResult::new("vm_lifecycle", iterations, &samples, failures)
        .with_metric("iterations_per_second", rate)
        .with_host_resources(resources.finish())
}

/// Executes and cleans up one complete lifecycle iteration.
pub(crate) fn lifecycle_once<A: VmApi>(
    api: &A,
    spec: &VmSpec,
    prefix: &str,
    sequence: u32,
) -> Result<Duration, BenchmarkError> {
    let started = Instant::now();
    let name = benchmark_name(prefix, sequence);
    let mut id = None;
    let result = (|| {
        let vm_id = api.create(spec, &name)?;
        id = Some(vm_id);
        start_and_wait(api, vm_id)?;
        api.stop(vm_id)?;
        wait_for_state(api, vm_id, VmState::Stopped, "stopped")?;
        start_and_wait(api, vm_id)?;
        api.stop(vm_id)?;
        wait_for_state(api, vm_id, VmState::Stopped, "stopped")?;
        api.delete(vm_id)?;
        id = None;
        Ok(started.elapsed())
    })();
    if result.is_err() {
        cleanup_if_created(api, id);
    }
    result
}

/// Boots a bounded group using scoped worker threads.
fn boot_group<A: VmApi>(
    api: &A,
    spec: &VmSpec,
    prefix: &str,
    first_sequence: u32,
    count: u32,
) -> Vec<Operation> {
    thread::scope(|scope| {
        let workers = (0..count)
            .map(|offset| {
                scope.spawn(move || boot_once(api, spec, prefix, first_sequence + offset))
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| {
                worker.join().unwrap_or(Operation {
                    id: None,
                    result: Err(BenchmarkError::WorkerPanic),
                })
            })
            .collect()
    })
}

/// Measures one VM from create request through running state.
fn boot_once<A: VmApi>(api: &A, spec: &VmSpec, prefix: &str, sequence: u32) -> Operation {
    let started = Instant::now();
    let name = benchmark_name(prefix, sequence);
    match api.create(spec, &name) {
        Ok(id) => Operation {
            id: Some(id),
            result: start_and_wait(api, id).map(|()| started.elapsed()),
        },
        Err(error) => Operation {
            id: None,
            result: Err(error.into()),
        },
    }
}

/// Starts one VM and waits until the API reports it running.
fn start_and_wait<A: VmApi>(api: &A, id: Uuid) -> Result<(), BenchmarkError> {
    api.start(id)?;
    wait_for_state(api, id, VmState::Running, "running")
}

/// Polls a VM until the requested state, failure, or timeout.
fn wait_for_state<A: VmApi>(
    api: &A,
    id: Uuid,
    expected: VmState,
    expected_name: &'static str,
) -> Result<(), BenchmarkError> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let current = api.state(id)?;
        if current == expected {
            return Ok(());
        }
        if current == VmState::Error && expected == VmState::Running {
            return Err(BenchmarkError::FailedBoot(id));
        }
        if Instant::now() >= deadline {
            return Err(BenchmarkError::Timeout {
                id,
                expected: expected_name,
            });
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Records one latency or failure and then removes its VM.
fn record_operation<A: VmApi>(
    api: &A,
    operation: Operation,
    sequence: u32,
    samples: &mut Vec<Duration>,
    failures: &mut Vec<String>,
) {
    match operation.result {
        Ok(elapsed) => samples.push(elapsed),
        Err(error) => failures.push(format!("iteration {sequence}: {error}")),
    }
    cleanup_if_created(api, operation.id);
}

/// Best-effort stop and delete for a possibly created benchmark VM.
fn cleanup_if_created<A: VmApi>(api: &A, id: Option<Uuid>) {
    let Some(id) = id else { return };
    if matches!(api.state(id), Ok(VmState::Running)) {
        let _ = api.stop(id);
    }
    let _ = api.delete(id);
}

/// Builds a recognizable VM name from the optional dashboard run tag.
fn benchmark_name(prefix: &str, sequence: u32) -> String {
    let run_tag = std::env::var("FIRECRAB_BENCH_RUN_TAG").ok().filter(|tag| {
        !tag.is_empty()
            && tag
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
    });
    benchmark_name_with_tag(prefix, sequence, run_tag.as_deref())
}

/// Builds a unique VM name with a validated tag supplied by the caller.
fn benchmark_name_with_tag(prefix: &str, sequence: u32, run_tag: Option<&str>) -> String {
    match run_tag {
        Some(tag) => format!(
            "bench-{prefix}-{tag}-{sequence}-{}",
            Uuid::new_v4().simple()
        ),
        None => format!("bench-{prefix}-{sequence}-{}", Uuid::new_v4().simple()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn benchmark_names_keep_the_optional_run_tag() {
        assert!(benchmark_name_with_tag("boot", 3, None).starts_with("bench-boot-3-"));
        assert!(
            benchmark_name_with_tag("boot", 3, Some("abc123")).starts_with("bench-boot-abc123-3-")
        );
    }

    struct FakeApi {
        states: Mutex<HashMap<Uuid, VmState>>,
        creates: AtomicU32,
        starts: AtomicU32,
        create_limit: Option<u32>,
    }

    impl FakeApi {
        fn new(create_limit: Option<u32>) -> Self {
            Self {
                states: Mutex::new(HashMap::new()),
                creates: AtomicU32::new(0),
                starts: AtomicU32::new(0),
                create_limit,
            }
        }

        fn remaining(&self) -> usize {
            self.states.lock().unwrap().len()
        }
    }

    impl VmApi for FakeApi {
        fn create(&self, _spec: &VmSpec, _name: &str) -> Result<Uuid, ApiError> {
            let sequence = self.creates.fetch_add(1, Ordering::SeqCst) + 1;
            if self.create_limit.is_some_and(|limit| sequence > limit) {
                return Err(ApiError::NotFound(Uuid::nil()));
            }
            let id = Uuid::from_u128(u128::from(sequence));
            self.states.lock().unwrap().insert(id, VmState::Created);
            Ok(id)
        }

        fn start(&self, id: Uuid) -> Result<(), ApiError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.states.lock().unwrap().insert(id, VmState::Running);
            Ok(())
        }

        fn state(&self, id: Uuid) -> Result<VmState, ApiError> {
            self.states
                .lock()
                .unwrap()
                .get(&id)
                .copied()
                .ok_or(ApiError::NotFound(id))
        }

        fn stop(&self, id: Uuid) -> Result<(), ApiError> {
            self.states.lock().unwrap().insert(id, VmState::Stopped);
            Ok(())
        }

        fn delete(&self, id: Uuid) -> Result<(), ApiError> {
            self.states.lock().unwrap().remove(&id);
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
    fn boot_runs_sequentially_and_cleans_up() {
        let api = FakeApi::new(None);
        let result = run_boot(&api, &spec(), 3);
        assert_eq!(result.successful_count, 3);
        assert_eq!(result.failed_count, 0);
        assert_eq!(api.remaining(), 0);
    }

    #[test]
    fn concurrent_creation_reports_rate_and_cleans_up() {
        let api = FakeApi::new(None);
        let result = run_concurrent_creation(&api, &spec(), 4);
        assert_eq!(result.successful_count, 4);
        assert!(result.metrics["vm_per_second"] > 0.0);
        assert_eq!(api.remaining(), 0);
    }

    #[test]
    fn density_reaches_the_requested_limit() {
        let api = FakeApi::new(None);
        let result = run_density(&api, &spec(), 5, 2, Duration::ZERO);
        assert_eq!(result.metrics["max_stable_microvms"], 5.0);
        assert_eq!(result.failed_count, 0);
        assert_eq!(api.remaining(), 0);
    }

    #[test]
    fn density_stops_after_a_failed_batch() {
        let api = FakeApi::new(Some(3));
        let result = run_density(&api, &spec(), 6, 2, Duration::ZERO);
        assert_eq!(result.metrics["max_stable_microvms"], 3.0);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.attempted_count, 4);
        assert_eq!(api.remaining(), 0);
    }

    #[test]
    fn lifecycle_performs_two_starts_per_iteration() {
        let api = FakeApi::new(None);
        let result = run_lifecycle(&api, &spec(), 3);
        assert_eq!(result.successful_count, 3);
        assert_eq!(api.starts.load(Ordering::SeqCst), 6);
        assert_eq!(api.remaining(), 0);
    }
}
