//! In-memory control plane for one host-local `firecrab-bench` process.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Maximum in-memory job snapshots retained by one API process.
const HISTORY_LIMIT: usize = 20;
/// Maximum combined stdout and stderr exposed to the dashboard.
const LOG_LIMIT_BYTES: usize = 32 * 1024;

/// Core benchmark command exposed by the dashboard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkCommand {
    /// Sequential VM boot latency.
    Boot,
    /// Concurrent VM creation and boot.
    Create,
    /// Maximum stable running VM count.
    Density,
    /// Repeated VM lifecycle operations.
    Lifecycle,
}

/// Validated options accepted by `POST /api/benchmark-jobs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartBenchmarkJobRequest {
    /// Benchmark algorithm.
    pub command: BenchmarkCommand,
    /// Installed image alias.
    pub template: String,
    /// MicroNetwork assigned to benchmark VMs.
    pub micro_network_id: Uuid,
    /// Guest memory in MiB.
    pub ram: u32,
    /// Guest vCPU count.
    pub cpu: u8,
    /// Guest disk size in GiB.
    pub disk_gb: u16,
    /// Boot sample count.
    pub count: Option<u32>,
    /// Concurrent creation worker count.
    pub concurrency: Option<u32>,
    /// Density upper bound.
    pub max_vms: Option<u32>,
    /// Density increment.
    pub step: Option<u32>,
    /// Lifecycle repetition count.
    pub iterations: Option<u32>,
    /// Explicit acknowledgement for the host-intensive density test.
    #[serde(default)]
    pub confirm_density: bool,
}

/// Lifecycle state of one benchmark child process.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkJobStatus {
    /// Child process is active.
    Running,
    /// Cancellation was requested.
    Cancelling,
    /// Child exited successfully and published its result.
    Succeeded,
    /// Child exited unsuccessfully or could not start.
    Failed,
    /// Child was cancelled by the operator.
    Cancelled,
}

/// Dashboard-facing snapshot of one benchmark execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkJob {
    /// Stable job identifier.
    pub id: Uuid,
    /// Requested command and limits.
    pub request: StartBenchmarkJobRequest,
    /// Current lifecycle state.
    pub status: BenchmarkJobStatus,
    /// Creation time as Unix milliseconds.
    pub created_at_ms: u64,
    /// Completion time as Unix milliseconds.
    pub finished_at_ms: Option<u64>,
    /// Bounded combined stdout and stderr.
    pub log: String,
}

#[derive(Debug)]
struct JobState {
    /// Recent jobs ordered newest first.
    jobs: VecDeque<BenchmarkJob>,
    /// Active job identifier and its one-shot cancellation sender.
    active: Option<(Uuid, Option<oneshot::Sender<()>>)>,
}

/// Cloneable tracker shared by API handlers.
#[derive(Debug, Clone)]
pub struct BenchmarkJobTracker {
    /// Shared mutable history and active-job slot.
    inner: Arc<Mutex<JobState>>,
    /// Host-local `firecrab-bench` executable.
    binary: PathBuf,
    /// Management API endpoint passed to the benchmark child.
    api_base: String,
}

/// Refusal to start a benchmark job.
#[derive(Debug, PartialEq, Eq)]
pub enum StartJobError {
    /// Another host-intensive benchmark is already active.
    AlreadyRunning,
    /// The installed benchmark executable is missing.
    BinaryUnavailable,
}

/// Refusal to cancel a benchmark job.
#[derive(Debug, PartialEq, Eq)]
pub enum CancelJobError {
    /// No job has this identifier.
    NotFound,
    /// The job already reached a terminal state.
    NotRunning,
}

impl BenchmarkJobTracker {
    /// Builds a tracker from the installed executable and API environment.
    pub fn from_env() -> Self {
        Self::new(
            resolve_binary(std::env::var_os("FIRECRAB_BENCH_BIN")),
            std::env::var("FIRECRAB_BENCH_API")
                .unwrap_or_else(|_| "http://127.0.0.1:5523".to_owned()),
        )
    }

    /// Builds a tracker with explicit dependencies for tests.
    pub fn new(binary: PathBuf, api_base: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(JobState {
                jobs: VecDeque::new(),
                active: None,
            })),
            binary,
            api_base: api_base.trim_end_matches('/').to_owned(),
        }
    }

    /// Starts one child process and returns its initial snapshot.
    pub fn start(&self, request: StartBenchmarkJobRequest) -> Result<BenchmarkJob, StartJobError> {
        if !is_executable(&self.binary) {
            return Err(StartJobError::BinaryUnavailable);
        }
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.active.is_some() {
            return Err(StartJobError::AlreadyRunning);
        }

        let id = Uuid::new_v4();
        let job = BenchmarkJob {
            id,
            request: request.clone(),
            status: BenchmarkJobStatus::Running,
            created_at_ms: unix_millis(),
            finished_at_ms: None,
            log: String::new(),
        };
        let (cancel_tx, cancel_rx) = oneshot::channel();
        state.jobs.push_front(job.clone());
        state.jobs.truncate(HISTORY_LIMIT);
        state.active = Some((id, Some(cancel_tx)));
        drop(state);

        let tracker = self.clone();
        tokio::spawn(async move { tracker.run(id, request, cancel_rx).await });
        Ok(job)
    }

    /// Newest job snapshots first.
    pub fn list(&self) -> Vec<BenchmarkJob> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jobs
            .iter()
            .cloned()
            .collect()
    }

    /// Returns one tracked job.
    pub fn get(&self, id: Uuid) -> Option<BenchmarkJob> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jobs
            .iter()
            .find(|job| job.id == id)
            .cloned()
    }

    /// Requests cancellation of the active child process.
    pub fn cancel(&self, id: Uuid) -> Result<BenchmarkJob, CancelJobError> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(job) = state.jobs.iter_mut().find(|job| job.id == id) else {
            return Err(CancelJobError::NotFound);
        };
        if job.status != BenchmarkJobStatus::Running {
            return Err(CancelJobError::NotRunning);
        }
        job.status = BenchmarkJobStatus::Cancelling;
        let snapshot = job.clone();
        if let Some((active_id, sender)) = state.active.as_mut()
            && *active_id == id
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(());
        }
        Ok(snapshot)
    }

    /// Owns the child process until completion or cancellation.
    async fn run(
        &self,
        id: Uuid,
        request: StartBenchmarkJobRequest,
        cancel: oneshot::Receiver<()>,
    ) {
        let arguments = command_arguments(&self.api_base, &request);
        let mut child = match Command::new(&self.binary)
            .args(&arguments)
            .env("FIRECRAB_BENCH_RUN_TAG", id.simple().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                self.finish(
                    id,
                    BenchmarkJobStatus::Failed,
                    format!("cannot run benchmark: {error}"),
                );
                return;
            }
        };
        let stdout = child.stdout.take().expect("piped benchmark stdout");
        let stderr = child.stderr.take().expect("piped benchmark stderr");
        let stdout_task = tokio::spawn(read_pipe(stdout));
        let stderr_task = tokio::spawn(read_pipe(stderr));
        let outcome = tokio::select! {
            result = child.wait() => {
                let stdout = stdout_task.await.unwrap_or_default();
                let stderr = stderr_task.await.unwrap_or_default();
                match result {
                    Ok(status) => {
                        let status = if status.success() { BenchmarkJobStatus::Succeeded } else { BenchmarkJobStatus::Failed };
                        (status, combined_log(&stdout, &stderr))
                    }
                    Err(error) => (BenchmarkJobStatus::Failed, format!("benchmark wait failed: {error}")),
                }
            },
            _ = cancel => {
                let _ = child.kill().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                let log = cleanup_cancelled_vms(&self.api_base, &id.simple().to_string()).await;
                (BenchmarkJobStatus::Cancelled, log)
            }
        };
        self.finish(id, outcome.0, outcome.1);
    }

    /// Publishes one terminal snapshot and releases the active-job slot.
    fn finish(&self, id: Uuid, status: BenchmarkJobStatus, log: String) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(job) = state.jobs.iter_mut().find(|job| job.id == id) {
            job.status = status;
            job.finished_at_ms = Some(unix_millis());
            job.log = truncate_log(log);
        }
        if state
            .active
            .as_ref()
            .is_some_and(|(active, _)| *active == id)
        {
            state.active = None;
        }
    }
}

/// Resolves the configured binary or the API executable's sibling binary.
fn resolve_binary(override_bin: Option<OsString>) -> PathBuf {
    if let Some(path) = override_bin
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("firecrab-bench")))
        .unwrap_or_else(|| PathBuf::from("/usr/local/lib/firecrab/firecrab-bench"))
}

/// Checks that a path names an executable regular file.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Converts a validated dashboard request into CLI arguments.
fn command_arguments(api_base: &str, request: &StartBenchmarkJobRequest) -> Vec<String> {
    let mut arguments = vec![
        "--api".to_owned(),
        api_base.to_owned(),
        "--publish".to_owned(),
    ];
    arguments.push(
        match request.command {
            BenchmarkCommand::Boot => "boot",
            BenchmarkCommand::Create => "create",
            BenchmarkCommand::Density => "density",
            BenchmarkCommand::Lifecycle => "lifecycle",
        }
        .to_owned(),
    );
    match request.command {
        BenchmarkCommand::Boot => {
            arguments.extend(["--count".to_owned(), request.count.unwrap().to_string()])
        }
        BenchmarkCommand::Create => arguments.extend([
            "--concurrency".to_owned(),
            request.concurrency.unwrap().to_string(),
        ]),
        BenchmarkCommand::Density => arguments.extend([
            "--max-vms".to_owned(),
            request.max_vms.unwrap().to_string(),
            "--step".to_owned(),
            request.step.unwrap().to_string(),
        ]),
        BenchmarkCommand::Lifecycle => arguments.extend([
            "--iterations".to_owned(),
            request.iterations.unwrap().to_string(),
        ]),
    }
    arguments.extend([
        "--template".to_owned(),
        request.template.clone(),
        "--micro-network-id".to_owned(),
        request.micro_network_id.to_string(),
        "--ram".to_owned(),
        request.ram.to_string(),
        "--cpu".to_owned(),
        request.cpu.to_string(),
        "--disk-gb".to_owned(),
        request.disk_gb.to_string(),
    ]);
    arguments
}

/// Combines lossy UTF-8 child output into one bounded dashboard log.
fn combined_log(stdout: &[u8], stderr: &[u8]) -> String {
    let mut log = String::from_utf8_lossy(stdout).into_owned();
    if !stderr.is_empty() {
        if !log.is_empty() && !log.ends_with('\n') {
            log.push('\n');
        }
        log.push_str(&String::from_utf8_lossy(stderr));
    }
    truncate_log(log)
}

/// Drains one child output pipe to prevent process backpressure.
async fn read_pipe(mut pipe: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes).await;
    bytes
}

/// Retains the newest complete UTF-8 suffix within the log limit.
fn truncate_log(log: String) -> String {
    if log.len() <= LOG_LIMIT_BYTES {
        return log;
    }
    let mut start = log.len() - LOG_LIMIT_BYTES;
    while !log.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &log[start..])
}

/// Returns the current wall-clock time as Unix milliseconds.
fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Debug, Deserialize)]
struct CleanupVm {
    /// VM identifier used by lifecycle endpoints.
    id: Uuid,
    /// VM name containing the unique benchmark run tag.
    name: String,
    /// Current API lifecycle state.
    state: String,
}

/// Removes only MicroVMs tagged for a cancelled dashboard run.
async fn cleanup_cancelled_vms(api_base: &str, run_tag: &str) -> String {
    let client = reqwest::Client::new();
    let marker = format!("-{run_tag}-");
    for _ in 0..20 {
        let response = match client.get(format!("{api_base}/api/vms")).send().await {
            Ok(response) => response,
            Err(error) => {
                return format!("benchmark cancelled; cleanup could not list VMs: {error}");
            }
        };
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return format!("benchmark cancelled; cleanup response failed: {error}"),
        };
        let vms = match serde_json::from_slice::<Vec<CleanupVm>>(&bytes) {
            Ok(vms) => vms,
            Err(error) => {
                return format!("benchmark cancelled; cleanup response was unreadable: {error}");
            }
        };
        let matching: Vec<_> = vms
            .into_iter()
            .filter(|vm| vm.name.contains(&marker))
            .collect();
        if matching.is_empty() {
            return "benchmark cancelled; tagged MicroVM cleanup complete".to_owned();
        }
        for vm in matching {
            let path = format!("{api_base}/api/vms/{}", vm.id);
            match vm.state.as_str() {
                "running" => {
                    let _ = client.post(format!("{path}/stop")).send().await;
                }
                "created" | "stopped" | "error" => {
                    let _ = client.delete(&path).send().await;
                }
                _ => {}
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    "benchmark cancelled; tagged MicroVM cleanup timed out—check the VM table".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn stub(root: &Path, script: &str) -> PathBuf {
        let path = root.join("firecrab-bench");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn request(command: BenchmarkCommand) -> StartBenchmarkJobRequest {
        StartBenchmarkJobRequest {
            command,
            template: "ubuntu-26.04".to_owned(),
            micro_network_id: Uuid::nil(),
            ram: 512,
            cpu: 1,
            disk_gb: 8,
            count: Some(5),
            concurrency: Some(10),
            max_vms: Some(20),
            step: Some(10),
            iterations: Some(100),
            confirm_density: true,
        }
    }

    #[test]
    fn builds_only_the_selected_commands_arguments() {
        let arguments =
            command_arguments("http://127.0.0.1:5523", &request(BenchmarkCommand::Density));
        assert!(arguments.windows(2).any(|pair| pair == ["--max-vms", "20"]));
        assert!(!arguments.iter().any(|argument| argument == "--count"));
        assert_eq!(arguments.last().map(String::as_str), Some("8"));
    }

    #[test]
    fn refuses_a_missing_binary_before_creating_a_job() {
        let tracker =
            BenchmarkJobTracker::new(PathBuf::from("/missing/firecrab-bench"), "api".to_owned());
        assert_eq!(
            tracker.start(request(BenchmarkCommand::Boot)),
            Err(StartJobError::BinaryUnavailable)
        );
        assert!(tracker.list().is_empty());
    }

    #[test]
    fn log_truncation_keeps_utf8_valid() {
        let log = "가".repeat(LOG_LIMIT_BYTES);
        let truncated = truncate_log(log);
        assert!(truncated.starts_with('…'));
        assert!(truncated.len() <= LOG_LIMIT_BYTES + '…'.len_utf8());
    }

    #[tokio::test]
    async fn successful_child_reaches_a_terminal_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let tracker = BenchmarkJobTracker::new(
            stub(root.path(), "echo completed"),
            "http://127.0.0.1:1".to_owned(),
        );
        let started = tracker.start(request(BenchmarkCommand::Boot)).unwrap();
        for _ in 0..20 {
            let job = tracker.get(started.id).unwrap();
            if job.status == BenchmarkJobStatus::Succeeded {
                assert!(job.log.contains("completed"));
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("stub benchmark did not finish");
    }

    #[tokio::test]
    async fn active_child_blocks_overlap_and_can_be_cancelled() {
        let root = tempfile::tempdir().unwrap();
        let tracker = BenchmarkJobTracker::new(
            stub(root.path(), "sleep 5"),
            "http://127.0.0.1:1".to_owned(),
        );
        let started = tracker.start(request(BenchmarkCommand::Boot)).unwrap();
        assert_eq!(
            tracker.start(request(BenchmarkCommand::Create)),
            Err(StartJobError::AlreadyRunning)
        );
        tracker.cancel(started.id).unwrap();
        for _ in 0..50 {
            if tracker.get(started.id).unwrap().status == BenchmarkJobStatus::Cancelled {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("stub benchmark was not cancelled");
    }
}
