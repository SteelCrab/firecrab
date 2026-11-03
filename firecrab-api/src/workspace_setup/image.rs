use super::*;
use crate::image_install::Architecture;
use crate::oci::{self, ImageReference, ImageVersion, Sha256Digest};
use crate::templates::TemplateSpec;
use firecrab_api_types::ImageInstallStatus;

const SOURCE: &str = "docker.io/library/ubuntu:26.04";

fn reference(job: &SavedSetup) -> Result<ImageReference, AppError> {
    ImageReference::parse(job.image_reference.as_deref().unwrap_or(SOURCE))
        .map_err(|_| failed(RequestId(job.progress.id)))
}

pub(super) fn template_alias(job: &SavedSetup) -> Result<Option<String>, AppError> {
    let Some(version) = job.kernel_version.as_deref() else {
        return Ok(None);
    };
    let named = oci::template_name_from_reference(&reference(job)?)
        .map_err(|_| failed(RequestId(job.progress.id)))?;
    Ok(Some(format!("{}-kernel-{version}", named.alias)))
}

pub(super) fn log(state: &AppState, job: &SavedSetup) -> String {
    let mut log = job.progress.log.clone();
    if let Ok(reference) = reference(job)
        && let Ok(named) = oci::template_name_from_reference(&reference)
    {
        log.push_str(&state.oci_imports.snapshot(&named.alias).log);
    }
    log
}

async fn record(state: &AppState, job: &mut SavedSetup, message: String) -> Result<(), AppError> {
    job.progress.log.push_str(&message);
    job.progress.log.push('\n');
    save(state, job).await
}

async fn preparation_failed(
    state: &AppState,
    job: &mut SavedSetup,
    error: impl std::fmt::Display,
) -> AppError {
    let _ = record(
        state,
        job,
        format!("Ubuntu OCI preparation failed: {error}"),
    )
    .await;
    conflict(
        "workspace_image_failed",
        "Ubuntu OCI preparation failed; inspect the preparation log and retry",
        RequestId(job.progress.id),
    )
}

/// Pin both the host's OCI manifest and the current stable kernel before creating a VM.
pub(super) async fn prepare(state: &AppState, job: &mut SavedSetup) -> Result<String, AppError> {
    let request = RequestId(job.progress.id);
    let mut source = reference(job)?;
    let credential =
        crate::handlers::oci::stored_credential_for(state, &source.registry, request).await?;
    if job.image_reference.is_none() {
        record(
            state,
            job,
            format!("Resolving {SOURCE} for {}", Architecture::HOST),
        )
        .await?;
        let resolved = match oci::resolve(
            &source,
            Architecture::HOST,
            oci::is_loopback_registry(&source.registry),
            credential.clone(),
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(error) => return Err(preparation_failed(state, job, error).await),
        };
        source.version = ImageVersion::Digest(
            Sha256Digest::parse(&resolved.digest).map_err(|_| failed(request))?,
        );
        job.image_reference = Some(format!(
            "{}/{}@{}",
            source.registry, source.repository, resolved.digest
        ));
        save(state, job).await?;
    }

    let requested = job.kernel_version.as_deref().unwrap_or("stable").to_owned();
    record(
        state,
        job,
        format!(
            "Preparing Linux kernel {requested} for {}",
            Architecture::HOST
        ),
    )
    .await?;
    let kernel = match oci::kernel::ensure_managed_kernel(
        state.templates.image_root_path(),
        Architecture::HOST,
        &requested,
        state.kernel_installs.base_url(),
        None,
    )
    .await
    {
        Ok(kernel) => kernel,
        Err(error) => return Err(preparation_failed(state, job, error).await),
    };
    job.kernel_version = crate::kernel_manager::version_for_path(&kernel);
    let alias = template_alias(job)?.ok_or_else(|| failed(request))?;
    save(state, job).await?;
    if state.templates.resolve_alias(&alias).is_some() {
        return Ok(alias);
    }

    let source_alias = oci::template_name_from_reference(&source)
        .map_err(|_| failed(request))?
        .alias;
    let operation = state.oci_imports.operation_lock(&source_alias);
    let _guard = operation.lock().await;
    if state.templates.resolve_alias(&source_alias).is_none() {
        if state
            .oci_imports
            .begin_with(&source_alias, "Ubuntu workspace OCI import")
            .is_ok()
        {
            oci::run_oci_import(
                state.oci_imports.clone(),
                (*state.templates).clone(),
                source,
                source_alias.clone(),
                credential,
            )
            .await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1800);
        while state.oci_imports.is_running(&source_alias) {
            if tokio::time::Instant::now() >= deadline {
                return Err(preparation_failed(state, job, "OCI import is still running").await);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if state.oci_imports.snapshot(&source_alias).status != ImageInstallStatus::Succeeded {
            return Err(preparation_failed(state, job, "OCI import failed; see import log").await);
        }
    }
    let original = state
        .templates
        .resolve_alias(&source_alias)
        .ok_or_else(|| failed(request))?;
    let spec = TemplateSpec {
        alias: alias.clone(),
        version: format!(
            "{}-kernel-{}",
            original.version,
            job.kernel_version.as_deref().unwrap()
        ),
        kernel,
        initrd: None,
        rootfs: original.rootfs.relative_path().to_owned(),
        boot_args: original.boot_args.clone(),
    };
    let templates = state.templates.clone();
    tokio::task::spawn_blocking(move || {
        if templates.resolve_alias(&spec.alias).is_none() {
            templates.register_spec(spec)?;
        }
        Ok::<_, crate::templates::TemplateError>(())
    })
    .await
    .map_err(|_| failed(request))?
    .map_err(|_| failed(request))?;
    record(
        state,
        job,
        format!("Ubuntu OCI image and Linux kernel are ready: {alias}"),
    )
    .await?;
    Ok(alias)
}
