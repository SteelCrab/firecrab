//! Async image install jobs: download template artifacts into the image root
//! from `FIRECRAB_IMAGE_BASE_URL`, verify, and hot-register them.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{ImageInstallResponse, ImageInstallStatus};
use tokio::io::AsyncWriteExt;

use crate::templates::{TemplateRegistry, TemplateSpec};

/// In-process install job tracker (one job per alias).
#[derive(Debug, Clone, Default)]
pub struct ImageInstallTracker {
    jobs: Arc<Mutex<HashMap<String, ImageInstallJob>>>,
    /// Base URL for artifact downloads (`FIRECRAB_IMAGE_BASE_URL`), trailing
    /// slash stripped. `None` when not configured — install refuses to start.
    base_url: Option<String>,
}

#[derive(Debug, Clone)]
struct ImageInstallJob {
    alias: String,
    status: ImageInstallStatus,
    log: Vec<String>,
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
}

impl ImageInstallTracker {
    /// Read `FIRECRAB_IMAGE_BASE_URL` (optional).
    pub fn from_env() -> Self {
        let base_url = env::var("FIRECRAB_IMAGE_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_owned())
            .filter(|value| !value.is_empty());
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            base_url,
        }
    }

    /// Test/helper constructor with an explicit base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        let base = base_url.into();
        let base_url = {
            let trimmed = base.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        };
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            base_url,
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn snapshot(&self, alias: &str) -> ImageInstallResponse {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        match jobs.get(alias) {
            Some(job) => job.to_response(),
            None => ImageInstallResponse {
                alias: alias.to_owned(),
                status: ImageInstallStatus::Idle,
                log: String::new(),
                started_at_ms: None,
                ended_at_ms: None,
            },
        }
    }

    /// Returns `Err("running")` if a job is already running for this alias.
    pub fn begin(&self, alias: &str) -> Result<ImageInstallResponse, &'static str> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = jobs.get(alias)
            && existing.status == ImageInstallStatus::Running
        {
            return Err("running");
        }
        let now = now_ms();
        let job = ImageInstallJob {
            alias: alias.to_owned(),
            status: ImageInstallStatus::Running,
            log: vec![format!("[{}] install started", clock(now))],
            started_at_ms: Some(now),
            ended_at_ms: None,
        };
        let response = job.to_response();
        jobs.insert(alias.to_owned(), job);
        Ok(response)
    }

    pub fn append_log(&self, alias: &str, line: impl Into<String>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = jobs.get_mut(alias) {
            job.log
                .push(format!("[{}] {}", clock(now_ms()), line.into()));
        }
    }

    pub fn finish_ok(&self, alias: &str) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = jobs.get_mut(alias) {
            let now = now_ms();
            job.log
                .push(format!("[{}] install succeeded — template registered", clock(now)));
            job.status = ImageInstallStatus::Succeeded;
            job.ended_at_ms = Some(now);
        }
    }

    pub fn finish_err(&self, alias: &str, detail: impl Into<String>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = jobs.get_mut(alias) {
            let now = now_ms();
            job.log
                .push(format!("[{}] install failed: {}", clock(now), detail.into()));
            job.status = ImageInstallStatus::Failed;
            job.ended_at_ms = Some(now);
        }
    }

    /// Drop install job state after a successful template delete.
    pub fn clear(&self, alias: &str) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.remove(alias);
    }

    /// `true` when an install for this alias is still running.
    pub fn is_running(&self, alias: &str) -> bool {
        let jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        jobs.get(alias)
            .is_some_and(|job| job.status == ImageInstallStatus::Running)
    }
}

impl ImageInstallJob {
    fn to_response(&self) -> ImageInstallResponse {
        ImageInstallResponse {
            alias: self.alias.clone(),
            status: self.status,
            log: self.log.join("\n"),
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clock(epoch_ms: u64) -> String {
    // Keep log timestamps local-friendly without pulling chrono: HH:MM:SS from
    // seconds-since-epoch is good enough for install progress.
    let secs = epoch_ms / 1000;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Download every artifact for `spec` into the registry image root, then
/// register it. Progress lines go to `tracker`.
pub async fn run_install(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    base_url: String,
    spec: TemplateSpec,
) {
    let alias = spec.alias.clone();
    if let Err(error) = install_once(&tracker, &templates, &base_url, &spec).await {
        tracker.finish_err(&alias, error);
        return;
    }
    tracker.finish_ok(&alias);
}

async fn install_once(
    tracker: &ImageInstallTracker,
    templates: &TemplateRegistry,
    base_url: &str,
    spec: &TemplateSpec,
) -> Result<(), String> {
    let root = templates.image_root_path().to_path_buf();
    let artifacts: Vec<&Path> = std::iter::once(spec.kernel.as_path())
        .chain(std::iter::once(spec.rootfs.as_path()))
        .chain(spec.initrd.as_deref())
        .collect();

    let client = reqwest::Client::builder()
        .user_agent(concat!("firecrab-api/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("http client: {error}"))?;

    for relative in artifacts {
        let rel = relative.to_string_lossy().replace('\\', "/");
        let url = format!("{base_url}/{rel}");
        tracker.append_log(&spec.alias, format!("downloading {rel}"));
        download_to(&client, &url, &root.join(relative)).await?;
        tracker.append_log(&spec.alias, format!("downloaded {rel}"));
    }

    tracker.append_log(&spec.alias, "verifying artifacts and registering");
    let templates = templates.clone();
    let spec = spec.clone();
    tokio::task::spawn_blocking(move || templates.register_spec(spec))
        .await
        .map_err(|error| format!("register task panicked: {error}"))?
        .map_err(|error| format!("register failed: {error}"))?;
    Ok(())
}

async fn download_to(client: &reqwest::Client, url: &str, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    let tmp = {
        let mut path = dest.as_os_str().to_owned();
        path.push(".partial");
        PathBuf::from(path)
    };

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", response.status()));
    }

    let mut file = tokio::fs::File::create(&tmp)
        .await
        .map_err(|error| format!("create {}: {error}", tmp.display()))?;
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("read body: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("write {}: {error}", tmp.display()))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("flush {}: {error}", tmp.display()))?;
    drop(file);

    tokio::fs::rename(&tmp, dest)
        .await
        .map_err(|error| format!("publish {}: {error}", dest.display()))?;
    Ok(())
}
