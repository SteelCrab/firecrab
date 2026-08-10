//! Async M2Image jobs: first download and structurally verify a per-alias
//! `.tar.zst` package from `FIRECRAB_IMAGE_BASE_URL`, then separately extract
//! its kernel/rootfs into the image root and hot-register the template.
//!
//! Layout (matches `scripts/package-m2images.sh` and the package registry):
//!
//! ```text
//! {FIRECRAB_IMAGE_BASE_URL}/{distro}/{version}/[aarch64/]{alias}.tar.zst
//! ```
//!
//! Unset / empty / `none` / `-` disables remote install (hosts that only use
//! pre-seeded `images/`).
//!
//! Archive members must be relative paths under `kernel/` or `rootfs/` only.

use std::collections::HashMap;
use std::env;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{ImageInstallResponse, ImageInstallStatus, PackageOrigin};
use tokio::io::AsyncWriteExt;

use crate::templates::{TemplateRegistry, TemplateSpec};

/// Default public MicroRegistry. Operators may override it for a private
/// registry, or set `FIRECRAB_IMAGE_BASE_URL=none` to disable remote access.
pub const DEFAULT_IMAGE_BASE_URL: &str = "https://registry.firecrab.dev";

/// Architecture label used by MicroRegistry catalog entries and object keys.
#[cfg(target_arch = "aarch64")]
pub const HOST_ARCHITECTURE: &str = "aarch64";
/// Architecture label used by MicroRegistry catalog entries and object keys.
#[cfg(target_arch = "x86_64")]
pub const HOST_ARCHITECTURE: &str = "x86_64";

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("Firecrab supports only x86_64 and aarch64 hosts");

pub const fn host_architecture() -> &'static str {
    HOST_ARCHITECTURE
}

/// Absolute package URL for a template alias under `base`.
pub fn package_url(base: &str, alias: &str) -> String {
    format!(
        "{}/{}",
        base.trim().trim_end_matches('/'),
        package_path(alias)
    )
}

/// In-process install job tracker (one job per alias).
#[derive(Debug, Clone, Default)]
pub struct ImageInstallTracker {
    jobs: Arc<Mutex<HashMap<String, ImageInstallJob>>>,
    /// Base URL for package downloads (`FIRECRAB_IMAGE_BASE_URL`), trailing
    /// slash stripped. `None` when remote install is disabled.
    base_url: Option<String>,
}

#[derive(Debug, Clone)]
struct ImageInstallJob {
    alias: String,
    status: ImageInstallStatus,
    log: Vec<String>,
    started_at_ms: Option<u64>,
    ended_at_ms: Option<u64>,
    downloaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
}

impl ImageInstallTracker {
    /// Read `FIRECRAB_IMAGE_BASE_URL`. Unset uses the public MicroRegistry;
    /// empty / `none` / `-` explicitly disables remote install.
    pub fn from_env() -> Self {
        match env::var("FIRECRAB_IMAGE_BASE_URL") {
            Ok(base_url) => Self::with_base_url(base_url),
            Err(env::VarError::NotPresent) => Self::with_base_url(DEFAULT_IMAGE_BASE_URL),
            Err(env::VarError::NotUnicode(_)) => Self::disabled(),
        }
    }

    /// Test/helper constructor with an explicit base URL.
    /// Empty / `none` / `-` disables remote install.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            base_url: normalize_base_url(&base_url.into()),
        }
    }

    /// Remote install disabled (no package base).
    pub fn disabled() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            base_url: None,
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// Package URL for `alias` when a base is configured.
    pub fn package_url_for(&self, alias: &str) -> Option<String> {
        self.base_url
            .as_deref()
            .map(|base| package_url(base, alias))
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
                downloaded_bytes: None,
                total_bytes: None,
            },
        }
    }

    /// Returns `Err("running")` if a job is already running for this alias.
    pub fn begin(&self, alias: &str) -> Result<ImageInstallResponse, &'static str> {
        self.begin_with(alias, "install started")
    }

    /// Start a job with a caller-owned label.  Package preparation and image
    /// installation share the same tracker mechanics but must be distinct in
    /// operator-visible logs.
    pub fn begin_with(
        &self,
        alias: &str,
        started_message: &str,
    ) -> Result<ImageInstallResponse, &'static str> {
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
            log: vec![format!("[{}] {started_message}", clock(now))],
            started_at_ms: Some(now),
            ended_at_ms: None,
            downloaded_bytes: None,
            total_bytes: None,
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

    /// Publish current streamed package bytes for a browser that reconnects
    /// or refreshes while the background job remains active.
    pub fn set_download_progress(
        &self,
        alias: &str,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
    ) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = jobs.get_mut(alias)
            && job.status == ImageInstallStatus::Running
        {
            job.downloaded_bytes = Some(downloaded_bytes);
            job.total_bytes = total_bytes;
        }
    }

    pub fn finish_ok(&self, alias: &str) {
        self.finish_ok_with(alias, "install succeeded — template registered");
    }

    /// Mark a job successful with a phase-specific completion message.
    /// Package acquisition and image registration are deliberately separate
    /// jobs, so their terminal logs must not claim the other phase ran.
    pub fn finish_ok_with(&self, alias: &str, message: impl Into<String>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = jobs.get_mut(alias) {
            let now = now_ms();
            job.log.push(format!("[{}] {}", clock(now), message.into()));
            job.status = ImageInstallStatus::Succeeded;
            job.ended_at_ms = Some(now);
        }
    }

    pub fn finish_err(&self, alias: &str, detail: impl Into<String>) {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(job) = jobs.get_mut(alias) {
            let now = now_ms();
            job.log.push(format!(
                "[{}] install failed: {}",
                clock(now),
                detail.into()
            ));
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
            downloaded_bytes: self.downloaded_bytes,
            total_bytes: self.total_bytes,
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

/// Package filename for a template alias (`ubuntu-26.04` → `ubuntu-26.04.tar.zst`).
pub fn package_name(alias: &str) -> String {
    format!("{alias}.tar.zst")
}

/// Public registry path for a known M2Image package. Unknown aliases keep the
/// historic flat layout so locally registered/custom templates remain usable.
pub fn package_path(alias: &str) -> String {
    package_path_for_arch(alias, host_architecture())
}

/// Registry path for `alias` on an explicit Firecracker host architecture.
/// x86_64 keeps the original layout; ARM64 uses a distinct subdirectory so
/// publishing one architecture can never replace the other's package.
pub fn package_path_for_arch(alias: &str, architecture: &str) -> String {
    let directory = match alias {
        "alpine-3.24" => Some("alpine/3.24"),
        "ubuntu-26.04" => Some("ubuntu/26.04"),
        "rocky-9" => Some("rocky/9.8"),
        _ => None,
    };
    match directory {
        Some(directory) if architecture == "aarch64" => {
            format!("{directory}/aarch64/{}", package_name(alias))
        }
        Some(directory) => format!("{directory}/{}", package_name(alias)),
        None => package_name(alias),
    }
}

/// Persistent local package cache. A package remains here after the image is
/// installed or removed so the two dashboard actions stay independent:
/// download/verify once, then install the image from that verified package.
pub fn staged_package_path(image_root: &Path, alias: &str) -> PathBuf {
    image_root.join(".packages").join(package_name(alias))
}

fn staged_package_origin_path(image_root: &Path, alias: &str) -> PathBuf {
    image_root
        .join(".packages")
        .join(format!("{}.origin", package_name(alias)))
}

/// Persist the producer next to a staged archive so dashboard ownership
/// remains correct across API restarts.
pub fn write_staged_package_origin(
    image_root: &Path,
    alias: &str,
    origin: PackageOrigin,
) -> std::io::Result<()> {
    let path = staged_package_origin_path(image_root, alias);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("origin.tmp");
    let value = match origin {
        PackageOrigin::MicroRegistry => "microRegistry\n",
        PackageOrigin::MicroBoot => "microBoot\n",
    };
    std::fs::write(&temporary, value)?;
    std::fs::rename(temporary, path)
}

/// Producer of the staged package, or `None` for legacy/unmarked archives.
pub fn staged_package_origin(image_root: &Path, alias: &str) -> Option<PackageOrigin> {
    let value = std::fs::read_to_string(staged_package_origin_path(image_root, alias)).ok()?;
    match value.trim() {
        "microRegistry" => Some(PackageOrigin::MicroRegistry),
        "microBoot" => Some(PackageOrigin::MicroBoot),
        _ => None,
    }
}

pub fn clear_staged_package_origin(image_root: &Path, alias: &str) -> std::io::Result<()> {
    match std::fs::remove_file(staged_package_origin_path(image_root, alias)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether an alias has a package ready for the separate image-install step.
pub fn staged_package_exists(image_root: &Path, alias: &str) -> bool {
    staged_package_path(image_root, alias).is_file()
}

/// Parse an operator-supplied base URL. Empty / `none` / `-` → disabled.
fn normalize_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") || trimmed == "-" {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Download and verify a package, leaving it in the persistent local package
/// cache for a later [`run_image_install`] call.
pub async fn run_package_install(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    base_url: String,
    spec: TemplateSpec,
) {
    let alias = spec.alias.clone();
    if let Err(error) = download_package_once(&tracker, &templates, &base_url, &spec).await {
        tracker.finish_err(&alias, format!("package preparation failed: {error}"));
        return;
    }
    tracker.finish_ok_with(&alias, "package ready — archive verified");
}

/// Extract and register an image from a package prepared by
/// [`run_package_install`].  This deliberately performs no network download.
pub async fn run_image_install(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    spec: TemplateSpec,
) {
    let alias = spec.alias.clone();
    if let Err(error) = install_staged_package_once(&tracker, &templates, &spec).await {
        tracker.finish_err(&alias, error);
        return;
    }
    tracker.finish_ok(&alias);
}

/// Backward-compatible combined operation for callers outside the dashboard.
/// The dashboard uses the two explicit operations above.
pub async fn run_install(
    tracker: ImageInstallTracker,
    templates: TemplateRegistry,
    base_url: String,
    spec: TemplateSpec,
) {
    let alias = spec.alias.clone();
    if let Err(error) = download_package_once(&tracker, &templates, &base_url, &spec).await {
        tracker.finish_err(&alias, error);
        return;
    }
    if let Err(error) = install_staged_package_once(&tracker, &templates, &spec).await {
        tracker.finish_err(&alias, error);
        return;
    }
    tracker.finish_ok(&alias);
}

async fn download_package_once(
    tracker: &ImageInstallTracker,
    templates: &TemplateRegistry,
    base_url: &str,
    spec: &TemplateSpec,
) -> Result<(), String> {
    let root = templates.image_root_path().to_path_buf();
    let package = package_name(&spec.alias);
    let remote_package = package_path(&spec.alias);
    let url = package_url(base_url, &spec.alias);
    let archive = staged_package_path(&root, &spec.alias);

    // Emit a real stage event before checking host prerequisites.  Otherwise
    // an unavailable `tar`/`zstd` binary leaves every Packer stage at
    // "waiting" even though this job already failed.
    tracker.append_log(&spec.alias, "[packer:source] preparing local package cache");
    ensure_host_tools()?;

    let client = reqwest::Client::builder()
        .user_agent(concat!("firecrab-api/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("http client: {error}"))?;

    // A final cache entry is only published after structural verification.
    // Otherwise a failed download/validation could look ready after restart.
    let temporary = archive.with_file_name(format!(".{package}.download"));
    let partial = partial_download_path(&temporary);
    let _ = tokio::fs::remove_file(&temporary).await;
    let _ = tokio::fs::remove_file(&partial).await;
    tracker.append_log(
        &spec.alias,
        format!("[packer:source] downloading {remote_package}"),
    );
    if let Err(error) = download_to_with_progress(&client, &url, &temporary, |downloaded, total| {
        tracker.set_download_progress(&spec.alias, downloaded, total);
    })
    .await
    {
        let _ = tokio::fs::remove_file(&temporary).await;
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(error);
    }
    tracker.append_log(
        &spec.alias,
        format!("[packer:source] downloaded {remote_package}"),
    );
    tracker.append_log(&spec.alias, "[packer:source] complete");

    if let Err(error) = verify_package_members(tracker, &temporary, spec).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(error);
    }
    tokio::fs::rename(&temporary, &archive)
        .await
        .map_err(|error| format!("publish {}: {error}", archive.display()))?;
    if let Err(error) =
        write_staged_package_origin(&root, &spec.alias, PackageOrigin::MicroRegistry)
    {
        let _ = tokio::fs::remove_file(&archive).await;
        return Err(format!("publish package origin: {error}"));
    }
    tracker.append_log(
        &spec.alias,
        "[packer:package] local package cache published",
    );
    tracker.append_log(&spec.alias, "[packer:package] complete");
    Ok(())
}

async fn verify_package_members(
    tracker: &ImageInstallTracker,
    archive: &Path,
    spec: &TemplateSpec,
) -> Result<(), String> {
    tracker.append_log(&spec.alias, "[packer:rootfs] checking rootfs artifact");
    let members = list_archive_members(archive).await?;
    validate_archive_members(&members)?;
    ensure_spec_members_present(spec, &members)?;
    tracker.append_log(
        &spec.alias,
        format!("[packer:rootfs] verified {}", spec.rootfs.display()),
    );
    tracker.append_log(&spec.alias, "[packer:rootfs] complete");
    tracker.append_log(
        &spec.alias,
        format!("[packer:kernel] verified {}", spec.kernel.display()),
    );
    if let Some(initrd) = &spec.initrd {
        tracker.append_log(
            &spec.alias,
            format!("[packer:kernel] verified {}", initrd.display()),
        );
    }
    tracker.append_log(&spec.alias, "[packer:kernel] complete");
    tracker.append_log(&spec.alias, "[packer:package] archive member check passed");
    Ok(())
}

async fn install_staged_package_once(
    tracker: &ImageInstallTracker,
    templates: &TemplateRegistry,
    spec: &TemplateSpec,
) -> Result<(), String> {
    let root = templates.image_root_path().to_path_buf();
    let archive = staged_package_path(&root, &spec.alias);

    ensure_host_tools()?;
    if !archive.is_file() {
        return Err(format!(
            "package is not ready for {} — run package install first",
            spec.alias
        ));
    }

    // Verify again immediately before extraction: the package may have been
    // prepared by an earlier API process and must not be trusted by path alone.
    verify_package_members(tracker, &archive, spec).await?;

    tracker.append_log(&spec.alias, "extracting into image root");
    extract_archive(&archive, &root).await?;
    tracker.append_log(&spec.alias, "extracted");

    tracker.append_log(&spec.alias, "verifying artifacts and registering");
    let templates = templates.clone();
    let spec = spec.clone();
    tokio::task::spawn_blocking(move || templates.register_spec(spec))
        .await
        .map_err(|error| format!("register task panicked: {error}"))?
        .map_err(|error| format!("register failed: {error}"))?;
    Ok(())
}

fn ensure_host_tools() -> Result<(), String> {
    for tool in ["tar", "zstd"] {
        if which(tool).is_none() {
            return Err(format!(
                "{tool} not found on PATH — install it to download template images \
                 (Debian/Ubuntu: apt install tar zstd)"
            ));
        }
    }
    Ok(())
}

fn which(bin: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(bin);
            candidate.is_file().then_some(candidate)
        })
    })
}

/// Members must be relative paths under `kernel/` or `rootfs/` with no `..`.
pub fn is_safe_archive_member(name: &str) -> bool {
    // tar may emit trailing slashes for directories.
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return false;
    }
    let path = Path::new(trimmed);
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    let first = first.to_string_lossy();
    if first != "kernel" && first != "rootfs" {
        return false;
    }
    for component in components {
        match component {
            Component::Normal(_) => {}
            _ => return false,
        }
    }
    true
}

fn validate_archive_members(members: &[String]) -> Result<(), String> {
    if members.is_empty() {
        return Err("archive is empty".to_owned());
    }
    for member in members {
        if !is_safe_archive_member(member) {
            return Err(format!(
                "refusing archive member `{member}` (only kernel/ and rootfs/ relative paths allowed)"
            ));
        }
    }
    Ok(())
}

fn ensure_spec_members_present(spec: &TemplateSpec, members: &[String]) -> Result<(), String> {
    let files: Vec<String> = members
        .iter()
        .filter(|m| !m.ends_with('/'))
        .map(|m| m.trim_end_matches('/').to_owned())
        .collect();
    let required: Vec<&Path> = std::iter::once(spec.kernel.as_path())
        .chain(std::iter::once(spec.rootfs.as_path()))
        .chain(spec.initrd.as_deref())
        .collect();
    for relative in required {
        let rel = relative.to_string_lossy().replace('\\', "/");
        if !files.iter().any(|f| f == &rel) {
            return Err(format!("archive missing required member `{rel}`"));
        }
    }
    Ok(())
}

async fn list_archive_members(archive: &Path) -> Result<Vec<String>, String> {
    let archive = archive.to_path_buf();
    tokio::task::spawn_blocking(move || list_archive_members_blocking(&archive))
        .await
        .map_err(|error| format!("list archive task panicked: {error}"))?
}

fn list_archive_members_blocking(archive: &Path) -> Result<Vec<String>, String> {
    let listed = Command::new("tar")
        .args(["--use-compress-program=zstd", "-tf"])
        .arg(archive)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let tar = match listed {
        Ok(output) if output.status.success() => output,
        Ok(_) | Err(_) => list_archive_members_piped_blocking(archive)?,
    };

    if !tar.status.success() {
        let err = String::from_utf8_lossy(&tar.stderr);
        return Err(format!("tar -t failed ({}): {}", tar.status, err.trim()));
    }

    Ok(String::from_utf8_lossy(&tar.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn list_archive_members_piped_blocking(archive: &Path) -> Result<std::process::Output, String> {
    let mut zstd = Command::new("zstd")
        .args(["-dc", "--"])
        .arg(archive)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn zstd: {error}"))?;

    let stdout = zstd
        .stdout
        .take()
        .ok_or_else(|| "zstd stdout missing".to_owned())?;

    let tar = Command::new("tar")
        .args(["-t"])
        .stdin(stdout)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("tar -t: {error}"))?;

    let zstd_status = zstd.wait().map_err(|error| format!("zstd wait: {error}"))?;
    if !zstd_status.success() {
        return Err(format!("zstd decompress failed ({zstd_status})"));
    }
    Ok(tar)
}

async fn extract_archive(archive: &Path, dest: &Path) -> Result<(), String> {
    let archive = archive.to_path_buf();
    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || extract_archive_blocking(&archive, &dest))
        .await
        .map_err(|error| format!("extract task panicked: {error}"))?
}

fn extract_archive_blocking(archive: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|error| format!("mkdir {}: {error}", dest.display()))?;

    let status = Command::new("tar")
        .args(["--use-compress-program=zstd", "-xf"])
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    match status {
        Ok(code) if code.success() => Ok(()),
        Ok(code) => extract_archive_piped_blocking(archive, dest).map_err(|piped| {
            format!("tar extract failed ({code}); pipe fallback also failed: {piped}")
        }),
        Err(error) => extract_archive_piped_blocking(archive, dest)
            .map_err(|piped| format!("tar spawn failed ({error}); pipe fallback: {piped}")),
    }
}

fn extract_archive_piped_blocking(archive: &Path, dest: &Path) -> Result<(), String> {
    let mut zstd = Command::new("zstd")
        .args(["-dc", "--"])
        .arg(archive)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn zstd: {error}"))?;

    let stdout = zstd
        .stdout
        .take()
        .ok_or_else(|| "zstd stdout missing".to_owned())?;

    let tar = Command::new("tar")
        .args(["-x", "-C"])
        .arg(dest)
        .stdin(stdout)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("tar -x: {error}"))?;

    let zstd_status = zstd.wait().map_err(|error| format!("zstd wait: {error}"))?;
    if !zstd_status.success() {
        return Err(format!("zstd decompress failed ({zstd_status})"));
    }
    if !tar.status.success() {
        let err = String::from_utf8_lossy(&tar.stderr);
        return Err(format!("tar -x failed ({}): {}", tar.status, err.trim()));
    }
    Ok(())
}

pub(crate) async fn download_to(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    download_to_with_progress(client, url, dest, |_downloaded, _total| {}).await
}

/// Stream an object to disk and report persisted byte counts after each
/// successful write. Package downloads use this for dashboard progress while
/// callers such as MicroBoot can keep using [`download_to`] with no tracker.
async fn download_to_with_progress<F>(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    mut report_progress: F,
) -> Result<(), String>
where
    F: FnMut(u64, Option<u64>),
{
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    }
    let tmp = partial_download_path(dest);

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", response.status()));
    }

    let total_bytes = response.content_length();
    let mut downloaded_bytes = 0_u64;
    report_progress(downloaded_bytes, total_bytes);

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
        downloaded_bytes = downloaded_bytes.saturating_add(chunk.len() as u64);
        report_progress(downloaded_bytes, total_bytes);
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

/// Work file used while streaming an archive download.  Keeping it separate
/// from the verified cache prevents a restart from mistaking a partial file
/// for a package that is ready to install.
fn partial_download_path(dest: &Path) -> PathBuf {
    let mut path = dest.as_os_str().to_owned();
    path.push(".partial");
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_api_types::ImageInstallStatus;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn tracker_begin_running_and_clear() {
        let tracker = ImageInstallTracker::with_base_url("http://example.test");
        assert!(tracker.base_url().is_some());
        assert!(!tracker.is_running("a"));

        let snap = tracker.begin("a").unwrap();
        assert_eq!(snap.status, ImageInstallStatus::Running);
        assert!(tracker.is_running("a"));
        assert_eq!(tracker.begin("a").unwrap_err(), "running");

        tracker.append_log("a", "step");
        tracker.append_log("missing", "ignored");
        tracker.set_download_progress("a", 512, Some(1024));
        let mid = tracker.snapshot("a");
        assert!(mid.log.contains("step"));
        assert_eq!(mid.downloaded_bytes, Some(512));
        assert_eq!(mid.total_bytes, Some(1024));

        tracker.finish_ok("a");
        assert!(!tracker.is_running("a"));
        assert_eq!(tracker.snapshot("a").status, ImageInstallStatus::Succeeded);

        tracker.begin("b").unwrap();
        tracker.finish_err("b", "boom");
        assert_eq!(tracker.snapshot("b").status, ImageInstallStatus::Failed);
        assert!(tracker.snapshot("b").log.contains("boom"));

        tracker.clear("a");
        assert_eq!(tracker.snapshot("a").status, ImageInstallStatus::Idle);
    }

    #[test]
    fn with_base_url_empty_or_none_disables() {
        assert!(
            ImageInstallTracker::with_base_url("   ")
                .base_url()
                .is_none()
        );
        assert!(
            ImageInstallTracker::with_base_url("none")
                .base_url()
                .is_none()
        );
        assert!(ImageInstallTracker::with_base_url("-").base_url().is_none());
        assert!(ImageInstallTracker::disabled().base_url().is_none());
    }

    #[test]
    fn package_url_uses_distribution_version_tree_for_known_aliases() {
        let architecture_segment = if host_architecture() == "aarch64" {
            "/aarch64"
        } else {
            ""
        };
        assert_eq!(
            package_url("http://127.0.0.1:8765/", "ubuntu-26.04"),
            format!(
                "http://127.0.0.1:8765/ubuntu/26.04{architecture_segment}/ubuntu-26.04.tar.zst"
            )
        );
        assert_eq!(
            package_url("http://mirror.example/m2", "alpine-3.24"),
            format!(
                "http://mirror.example/m2/alpine/3.24{architecture_segment}/alpine-3.24.tar.zst"
            )
        );
        assert_eq!(
            package_url("http://mirror.example/m2", "rocky-9"),
            format!("http://mirror.example/m2/rocky/9.8{architecture_segment}/rocky-9.tar.zst")
        );
        let tracker = ImageInstallTracker::with_base_url("http://127.0.0.1:8765");
        let expected_ubuntu_url = format!(
            "http://127.0.0.1:8765/ubuntu/26.04{architecture_segment}/ubuntu-26.04.tar.zst"
        );
        assert_eq!(
            tracker.package_url_for("ubuntu-26.04").as_deref(),
            Some(expected_ubuntu_url.as_str())
        );
        assert!(
            ImageInstallTracker::disabled()
                .package_url_for("ubuntu-26.04")
                .is_none()
        );
    }

    #[test]
    fn arm64_packages_use_architecture_specific_registry_paths() {
        assert_eq!(
            package_path_for_arch("alpine-3.24", "aarch64"),
            "alpine/3.24/aarch64/alpine-3.24.tar.zst"
        );
        assert_eq!(
            package_path_for_arch("ubuntu-26.04", "aarch64"),
            "ubuntu/26.04/aarch64/ubuntu-26.04.tar.zst"
        );
        assert_eq!(
            package_path_for_arch("ubuntu-26.04", "x86_64"),
            "ubuntu/26.04/ubuntu-26.04.tar.zst"
        );
        assert_eq!(
            package_path_for_arch("rocky-9", "aarch64"),
            "rocky/9.8/aarch64/rocky-9.tar.zst"
        );
        assert_eq!(
            package_path_for_arch("rocky-9", "x86_64"),
            "rocky/9.8/rocky-9.tar.zst"
        );
    }

    #[test]
    fn finish_on_missing_job_is_noop() {
        let tracker = ImageInstallTracker::default();
        tracker.finish_ok("nope");
        tracker.finish_err("nope", "x");
        assert_eq!(tracker.snapshot("nope").status, ImageInstallStatus::Idle);
    }

    #[test]
    fn clock_formats_hh_mm_ss() {
        // 1h 2m 3s past epoch → 01:02:03
        assert_eq!(clock(3_723_000), "01:02:03");
    }

    #[test]
    fn package_name_uses_alias_tar_zst() {
        assert_eq!(package_name("alpine-3.24"), "alpine-3.24.tar.zst");
        assert_eq!(package_name("ubuntu-26.04"), "ubuntu-26.04.tar.zst");
        assert_eq!(package_name("rocky-9"), "rocky-9.tar.zst");
    }

    #[test]
    fn staged_package_origin_round_trips_and_clears() {
        let directory = tempdir().unwrap();
        write_staged_package_origin(
            directory.path(),
            "alpine-3.24",
            PackageOrigin::MicroRegistry,
        )
        .unwrap();
        assert_eq!(
            staged_package_origin(directory.path(), "alpine-3.24"),
            Some(PackageOrigin::MicroRegistry)
        );
        clear_staged_package_origin(directory.path(), "alpine-3.24").unwrap();
        assert_eq!(staged_package_origin(directory.path(), "alpine-3.24"), None);
    }

    #[test]
    fn package_path_keeps_custom_aliases_flat() {
        assert_eq!(package_path("derived-nginx"), "derived-nginx.tar.zst");
    }

    #[test]
    fn safe_member_allows_kernel_and_rootfs() {
        assert!(is_safe_archive_member("kernel/vmlinux"));
        assert!(is_safe_archive_member("rootfs/root.ext4"));
        assert!(is_safe_archive_member("kernel/nested/file"));
        assert!(is_safe_archive_member("rootfs/dir/"));
    }

    #[test]
    fn safe_member_rejects_traversal_and_other_roots() {
        assert!(!is_safe_archive_member("../kernel/x"));
        assert!(!is_safe_archive_member("kernel/../x"));
        assert!(!is_safe_archive_member("/kernel/x"));
        assert!(!is_safe_archive_member("etc/passwd"));
        assert!(!is_safe_archive_member(""));
        // Top-level `kernel` / `rootfs` directory entries from tar are fine.
        assert!(is_safe_archive_member("kernel"));
        assert!(is_safe_archive_member("rootfs/"));
        assert!(!is_safe_archive_member("data/vms/x"));
    }

    #[test]
    fn validate_rejects_bad_member() {
        let err = validate_archive_members(&["kernel/ok".into(), "../evil".into()]).unwrap_err();
        assert!(err.contains("../evil"));
    }

    #[test]
    fn validation_accepts_a_complete_spec_and_rejects_an_empty_archive() {
        let alpine = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        let members = vec![
            "kernel/".to_owned(),
            alpine.kernel.to_string_lossy().into_owned(),
            alpine
                .initrd
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            alpine.rootfs.to_string_lossy().into_owned(),
        ];

        validate_archive_members(&members).unwrap();
        ensure_spec_members_present(&alpine, &members).unwrap();
        assert_eq!(
            validate_archive_members(&[]).unwrap_err(),
            "archive is empty"
        );
    }

    #[test]
    fn ensure_spec_requires_all_artifacts() {
        let alpine = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        let members = vec![
            alpine.kernel.to_string_lossy().into_owned(),
            alpine.rootfs.to_string_lossy().into_owned(),
            // missing initrd
        ];
        let err = ensure_spec_members_present(&alpine, &members).unwrap_err();
        assert!(err.contains("initramfs") || err.contains("missing"));
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn make_tar_zst(source: &Path, members: &[&str], dest: &Path) {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let tar = Command::new("tar")
            .args(["-C"])
            .arg(source)
            .arg("-cf")
            .arg("-")
            .args(members)
            .stdout(Stdio::piped())
            .spawn()
            .expect("tar");
        let status = Command::new("zstd")
            .args(["-q", "-f", "-o"])
            .arg(dest)
            .stdin(tar.stdout.unwrap())
            .status()
            .expect("zstd");
        assert!(status.success(), "zstd failed");
    }

    #[tokio::test]
    async fn extract_safe_archive_places_files() {
        let source = tempdir().unwrap();
        let dest = tempdir().unwrap();
        write_file(&source.path().join("kernel/vmlinux"), b"kernel-bytes");
        write_file(&source.path().join("rootfs/root.ext4"), b"rootfs-bytes");
        let archive = source.path().join("demo.tar.zst");
        make_tar_zst(
            source.path(),
            &["kernel/vmlinux", "rootfs/root.ext4"],
            &archive,
        );

        let members = list_archive_members(&archive).await.unwrap();
        validate_archive_members(&members).unwrap();
        extract_archive(&archive, dest.path()).await.unwrap();
        assert_eq!(
            fs::read(dest.path().join("kernel/vmlinux")).unwrap(),
            b"kernel-bytes"
        );
        assert_eq!(
            fs::read(dest.path().join("rootfs/root.ext4")).unwrap(),
            b"rootfs-bytes"
        );
    }

    #[test]
    fn piped_tar_fallback_lists_and_extracts_a_valid_archive() {
        let source = tempdir().unwrap();
        let dest = tempdir().unwrap();
        write_file(&source.path().join("kernel/vmlinux"), b"kernel-bytes");
        write_file(&source.path().join("rootfs/root.ext4"), b"rootfs-bytes");
        let archive = source.path().join("demo.tar.zst");
        make_tar_zst(
            source.path(),
            &["kernel/vmlinux", "rootfs/root.ext4"],
            &archive,
        );

        let listed = list_archive_members_piped_blocking(&archive).unwrap();
        assert!(listed.status.success());
        assert!(String::from_utf8_lossy(&listed.stdout).contains("kernel/vmlinux"));

        extract_archive_piped_blocking(&archive, dest.path()).unwrap();
        assert_eq!(
            fs::read(dest.path().join("rootfs/root.ext4")).unwrap(),
            b"rootfs-bytes"
        );
    }

    #[tokio::test]
    async fn list_rejects_when_member_escapes() {
        let source = tempdir().unwrap();
        // Craft a tar with a bad path using GNU tar --transform is awkward;
        // write a member under `etc/` which fails our allowlist.
        write_file(&source.path().join("etc/passwd"), b"nope");
        let archive = source.path().join("bad.tar.zst");
        make_tar_zst(source.path(), &["etc/passwd"], &archive);
        let members = list_archive_members(&archive).await.unwrap();
        let err = validate_archive_members(&members).unwrap_err();
        assert!(err.contains("etc/passwd"));
    }

    #[tokio::test]
    async fn image_install_reports_failure_when_no_package_has_been_staged() {
        let directory = tempdir().unwrap();
        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty()).unwrap();
        let spec = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        let tracker = ImageInstallTracker::disabled();
        tracker.begin(&spec.alias).unwrap();

        run_image_install(tracker.clone(), templates, spec.clone()).await;

        let snapshot = tracker.snapshot(&spec.alias);
        assert_eq!(snapshot.status, ImageInstallStatus::Failed);
        assert!(snapshot.log.contains("package is not ready"));
    }

    #[tokio::test]
    async fn package_install_reports_a_download_failure_without_publishing_a_cache_entry() {
        let directory = tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().fallback(|| async { axum::http::StatusCode::NOT_FOUND }),
            )
            .await
            .ok();
        });

        let templates = TemplateRegistry::from_specs(directory.path(), std::iter::empty()).unwrap();
        let spec = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        let tracker = ImageInstallTracker::with_base_url(format!("http://{addr}"));
        tracker.begin(&spec.alias).unwrap();

        run_package_install(
            tracker.clone(),
            templates.clone(),
            format!("http://{addr}"),
            spec.clone(),
        )
        .await;

        let snapshot = tracker.snapshot(&spec.alias);
        assert_eq!(snapshot.status, ImageInstallStatus::Failed);
        assert!(snapshot.log.contains("package preparation failed"));
        assert!(!staged_package_path(templates.image_root_path(), &spec.alias).exists());
    }

    #[tokio::test]
    async fn install_once_from_local_http_registers() {
        let source = tempdir().unwrap();
        let dest = tempdir().unwrap();
        let alpine = TemplateRegistry::known_spec("alpine-3.24").unwrap();
        write_file(&source.path().join(&alpine.kernel), b"fake-alpine-kernel");
        write_file(
            &source.path().join(alpine.initrd.as_ref().unwrap()),
            b"fake-alpine-initrd",
        );
        write_file(&source.path().join(&alpine.rootfs), b"fake-alpine-rootfs");

        let members = [
            alpine.kernel.to_str().unwrap(),
            alpine.initrd.as_ref().unwrap().to_str().unwrap(),
            alpine.rootfs.to_str().unwrap(),
        ];
        let archive_path = source.path().join(package_path(&alpine.alias));
        fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        make_tar_zst(source.path(), &members, &archive_path);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback_service(tower_http::services::ServeDir::new(
            source.path().to_path_buf(),
        ));
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let templates = TemplateRegistry::from_specs(dest.path(), std::iter::empty()).unwrap();
        let tracker = ImageInstallTracker::with_base_url(format!("http://{addr}"));
        tracker.begin(&alpine.alias).unwrap();
        run_install(
            tracker.clone(),
            templates.clone(),
            format!("http://{addr}"),
            alpine.clone(),
        )
        .await;
        let snap = tracker.snapshot(&alpine.alias);
        assert_eq!(
            snap.status,
            ImageInstallStatus::Succeeded,
            "log:\n{}",
            snap.log
        );
        assert_eq!(
            staged_package_origin(dest.path(), &alpine.alias),
            Some(PackageOrigin::MicroRegistry)
        );
        assert!(templates.resolve_alias("alpine-3.24").is_some());
    }
}
