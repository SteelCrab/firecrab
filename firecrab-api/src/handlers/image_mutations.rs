//! Durable native image admission, frozen preparation and original-operation recovery.
use super::{iac, images, stacks::persistence_error};
use crate::{
    error::AppError,
    extract::ValidatedJson,
    image_install::{self, Architecture},
    image_lifecycle::{self, ImageAuthority, ImageWork},
    persistence::iac_images::{
        ImageArtifact, ImageDefinition, ImageIntent, ImageOutcome, ImageRequest, ImageSource,
    },
    server::RequestId,
    state::AppState,
    templates::{TemplateRegistry, TemplateSpec},
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use firecrab_api_types::{ImageInstallResponse, ImageInstallStatus, UpdateImageKernelRequest};
use firecrab_stack::{NativeAction, NativeOperation, NativeOperationState};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, os::unix::fs::OpenOptionsExt};
use uuid::Uuid;

fn unavailable(id: Uuid) -> AppError {
    AppError::unavailable("Inspect and recover the original image operation", id)
}
pub async fn install(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(rid): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    route(state, rid, headers, ImageRequest::Install { alias }).await
}
pub async fn import(
    State(state): State<AppState>,
    Extension(rid): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<firecrab_api_types::OciImportRequest>,
) -> Response {
    route(
        state,
        rid,
        headers,
        ImageRequest::Import {
            reference: body.reference,
        },
    )
    .await
}
#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeleteImageQuery {
    #[serde(default)]
    pub retain_artifacts: bool,
}
pub async fn delete(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(rid): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<DeleteImageQuery>,
) -> Response {
    route(
        state,
        rid,
        headers,
        ImageRequest::Delete {
            alias,
            retain_artifacts: query.retain_artifacts,
        },
    )
    .await
}
pub async fn kernel(
    State(state): State<AppState>,
    Path(alias): Path<String>,
    Extension(rid): Extension<RequestId>,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<UpdateImageKernelRequest>,
) -> Response {
    route(
        state,
        rid,
        headers,
        ImageRequest::Kernel {
            alias,
            version: body.kernel_version.trim().into(),
        },
    )
    .await
}
pub(crate) fn snapshot(state: &AppState, intent: &ImageIntent) -> ImageInstallResponse {
    let mut value = state.image_installs.snapshot(&intent.definition().alias);
    value.status = match intent.operation.state {
        NativeOperationState::Running => ImageInstallStatus::Running,
        NativeOperationState::Succeeded => ImageInstallStatus::Succeeded,
        _ => ImageInstallStatus::Failed,
    };
    // Durable state takes precedence over a tracker from an earlier attempt.
    value.log = match intent.operation.state {
        NativeOperationState::Running => "Original image operation is running",
        NativeOperationState::Succeeded => "Original image operation completed",
        NativeOperationState::NeedsRecovery => "Recover the original image operation",
        NativeOperationState::Failed => "Original image operation failed before changes",
    }
    .into();
    value
}
async fn route(
    state: AppState,
    rid: RequestId,
    headers: HeaderMap,
    request: ImageRequest,
) -> Response {
    let id = match iac::operation_id(&headers, rid.0) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let observed = state.clone();
    let result = tokio::spawn(async move { admit(state, rid, headers, id, request).await }).await;
    let mut response = match result {
        Ok(Ok(intent)) => match intent.operation.state {
            NativeOperationState::Succeeded => match observed.store.image_receipt(id) {
                Ok(Some(outcome)) if !outcome.installed => StatusCode::NO_CONTENT.into_response(),
                Ok(Some(outcome)) if intent.operation.action == NativeAction::UpdateImageKernel => Json(outcome.image).into_response(),
                Ok(Some(_)) => Json(snapshot(&observed, &intent)).into_response(),
                _ => AppError::internal(rid.0).into_response(),
            },
            NativeOperationState::Running => (StatusCode::ACCEPTED, Json(snapshot(&observed, &intent))).into_response(),
            NativeOperationState::NeedsRecovery => AppError::conflict("image_recovery_required", "Recover the original image operation before changing this alias", rid.0).into_response(),
            NativeOperationState::Failed => AppError::new(StatusCode::BAD_GATEWAY, "image_unstarted", "Image preparation failed before changes; correct its prerequisites and use a new operation ID", rid.0).into_response(),
        },
        Ok(Err(error)) => error.into_response(),
        Err(_) => AppError::internal(rid.0).into_response(),
    };
    response.headers_mut().insert(
        "x-firecrab-operation-id",
        HeaderValue::from_str(&id.to_string()).expect("UUID header"),
    );
    response
}
async fn admit(
    state: AppState,
    rid: RequestId,
    headers: HeaderMap,
    id: Uuid,
    request: ImageRequest,
) -> Result<ImageIntent, AppError> {
    if let Some(expected) = headers.get("x-firecrab-endpoint-id")
        && expected
            .to_str()
            .ok()
            .and_then(|value| Uuid::parse_str(value).ok())
            != Some(
                state
                    .store
                    .iac_endpoint_id()
                    .map_err(|_| AppError::internal(rid.0))?,
            )
    {
        return Err(persistence_error(
            crate::persistence::PersistenceError::IacEndpointConflict,
            rid.0,
        ));
    }
    if let Some(intent) = state
        .store
        .image_retry(id, &request)
        .map_err(|error| persistence_error(error, rid.0))?
    {
        return Ok(intent);
    }
    let alias = match &request {
        ImageRequest::Install { alias }
        | ImageRequest::Kernel { alias, .. }
        | ImageRequest::Delete { alias, .. } => alias.clone(),
        ImageRequest::Import { reference } => {
            let reference = crate::oci::ImageReference::parse(reference).map_err(|_| {
                AppError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_reference",
                    "Invalid OCI reference",
                    rid.0,
                )
            })?;
            crate::oci::claim_template_name(&reference, &state.templates)
                .map_err(|_| {
                    AppError::conflict(
                        "alias_collision",
                        "Derived OCI alias is already installed or reserved",
                        rid.0,
                    )
                })?
                .alias
        }
    };
    if !crate::persistence::iac_images::valid_alias(&alias) {
        return Err(AppError::not_found(rid.0));
    }
    if !state
        .store
        .iac_public_write_allowed(&crate::persistence::iac_images::resource(&alias))
        .map_err(|error| persistence_error(error, rid.0))?
    {
        return Err(iac::managed(rid.0));
    }
    let guard = image_lifecycle::lock(&state.templates).map_err(|_| unavailable(rid.0))?;
    let versions = state
        .templates
        .observed_images(&alias)
        .map_err(|_| unavailable(rid.0))?;
    let before = versions.last().cloned();
    if state
        .store
        .deploy_images()
        .map_err(|_| unavailable(rid.0))?
        .iter()
        .any(|owned| {
            versions
                .iter()
                .any(|version| version.spec.rootfs == owned.rootfs)
        })
    {
        return Err(persistence_error(
            crate::persistence::PersistenceError::IacParentManaged,
            rid.0,
        ));
    }
    if state.image_installs.is_running(&alias)
        || state.image_packages.is_running(&alias)
        || state.oci_imports.is_running(&alias)
    {
        return Err(AppError::conflict(
            "image_operation_in_progress",
            "Wait for the original image operation",
            rid.0,
        ));
    }
    if matches!(
        request,
        ImageRequest::Delete { .. } | ImageRequest::Kernel { .. }
    ) {
        super::pools::ensure_image_unused(&state, &alias, rid).await?;
    }
    let mut dependencies = Vec::new();
    let source = match &request {
        ImageRequest::Install { .. } => {
            if state.templates.resolve_alias(&alias).is_some() {
                return Err(AppError::conflict(
                    "already_installed",
                    "Image is already installed",
                    rid.0,
                ));
            }
            let spec = match TemplateRegistry::known_spec(&alias) {
                Some(spec) => spec,
                None => {
                    let entry = state
                        .store
                        .microregistry_local(&alias, Architecture::HOST.as_str())
                        .map_err(|_| AppError::internal(rid.0))?
                        .ok_or_else(|| AppError::not_found(rid.0))?;
                    crate::package::read_local_template_spec(
                        state.templates.image_root_path(),
                        &entry,
                    )
                    .map_err(|_| unavailable(rid.0))?
                }
            };
            let archive =
                image_install::staged_package_path(state.templates.image_root_path(), &alias);
            let archive_sha256 = tokio::task::spawn_blocking(move || checksum(&archive))
                .await
                .map_err(|_| unavailable(rid.0))?
                .map_err(|_| {
                    AppError::conflict(
                        "package_required",
                        "Prepare the local image package first",
                        rid.0,
                    )
                })?;
            ImageSource::Package {
                spec,
                archive_sha256,
            }
        }
        ImageRequest::Kernel { version, .. } => {
            let current = state.templates.resolve_alias(&alias).ok_or_else(|| {
                AppError::conflict("not_installed", "Image is not installed", rid.0)
            })?;
            let kernel = crate::kernel_manager::cache_path(version).ok_or_else(|| {
                AppError::conflict(
                    "kernel_required",
                    "Select an installed managed kernel",
                    rid.0,
                )
            })?;
            let lease = crate::kernel_lifecycle::borrow_existing(&state.templates, version)
                .await
                .map_err(|_| unavailable(rid.0))?;
            let spec = TemplateSpec {
                alias: alias.clone(),
                version: if current.kernel.relative_path() == kernel {
                    current.version.clone()
                } else {
                    format!("{}-kernel-{}", current.version, version)
                },
                kernel,
                initrd: current
                    .initrd
                    .as_ref()
                    .map(|value| value.relative_path().to_owned()),
                rootfs: current.rootfs.relative_path().to_owned(),
                boot_args: current.boot_args.clone(),
            };
            let pin = state
                .templates
                .preview_image(&spec)
                .map_err(|_| unavailable(rid.0))?
                .stack_pin();
            lease
                .check(state.templates.image_root_path())
                .map_err(|_| unavailable(rid.0))?;
            dependencies.push(crate::persistence::iac_kernels::resource(
                Architecture::HOST.as_str(),
                version,
            ));
            ImageSource::Kernel {
                spec,
                version: version.clone(),
                pin,
            }
        }
        ImageRequest::Delete {
            retain_artifacts, ..
        } => {
            if before.is_none() {
                return Err(AppError::conflict(
                    "not_installed",
                    "Image is not installed",
                    rid.0,
                ));
            }
            let mut artifacts = Vec::new();
            for version in &versions {
                if let Some(pin) = &version.pin {
                    artifacts.push(ImageArtifact {
                        relative: version.spec.kernel.clone(),
                        sha256: pin.kernel_sha256.clone(),
                    });
                    artifacts.push(ImageArtifact {
                        relative: version.spec.rootfs.clone(),
                        sha256: pin.rootfs_sha256.clone(),
                    });
                    if let (Some(relative), Some(sha256)) =
                        (&version.spec.initrd, &pin.initrd_sha256)
                    {
                        artifacts.push(ImageArtifact {
                            relative: relative.clone(),
                            sha256: sha256.clone(),
                        });
                    }
                } else if !retain_artifacts {
                    for relative in std::iter::once(&version.spec.kernel)
                        .chain(std::iter::once(&version.spec.rootfs))
                        .chain(version.spec.initrd.iter())
                    {
                        let templates = state.templates.clone();
                        let relative = relative.clone();
                        let candidate = tokio::task::spawn_blocking(move || {
                            templates.image_artifact_digest(&relative).map(|digest| {
                                digest.map(|sha256| ImageArtifact { relative, sha256 })
                            })
                        })
                        .await
                        .map_err(|_| unavailable(rid.0))?
                        .map_err(|_| unavailable(rid.0))?;
                        if let Some(candidate) = candidate {
                            artifacts.push(candidate);
                        }
                    }
                }
            }
            artifacts.sort_by(|a, b| a.relative.cmp(&b.relative));
            artifacts.dedup_by(|a, b| a.relative == b.relative && a.sha256 == b.sha256);
            ImageSource::Delete {
                versions: versions.clone(),
                artifacts,
                retain_artifacts: *retain_artifacts,
            }
        }
        ImageRequest::Import { reference } => {
            let original = crate::oci::ImageReference::parse(reference)
                .map_err(|_| AppError::not_found(rid.0))?;
            let (credential, authentication) = import_auth(&state, &original.registry, rid).await?;
            if let Some(pin) = &authentication {
                dependencies.push(pin.ownership.resource.clone());
            }
            let login = credential
                .as_ref()
                .map(|login| crate::oci::RegistryCredential {
                    registry: login.registry.clone(),
                    username: login.username.clone(),
                    secret: login.secret.clone(),
                });
            let resolved = crate::oci::resolve(
                &original,
                Architecture::HOST,
                crate::oci::is_loopback_registry(&original.registry),
                login,
            )
            .await
            .map_err(|_| unavailable(rid.0))?;
            let named = crate::oci::template_name_from_reference(&original)
                .map_err(|_| unavailable(rid.0))?;
            let (kernel_version, kernel_override) =
                crate::oci::capture_import_kernel(&state.templates)
                    .await
                    .map_err(|_| unavailable(rid.0))?;
            ImageSource::Oci {
                reference: format!(
                    "{}/{}@{}",
                    original.registry, original.repository, resolved.digest
                ),
                naming_reference: reference.clone(),
                display_version: named.version,
                kernel_version,
                kernel_override,
                credential,
                authentication,
            }
        }
    };
    let definition = ImageDefinition {
        alias,
        architecture: Architecture::HOST.as_str().into(),
        root: state
            .templates
            .physical_root()
            .map_err(|_| unavailable(rid.0))?,
        before,
        source,
        dependencies,
    };
    let (intent, dispatch) = state
        .store
        .begin_image(id, request, definition)
        .map_err(|error| persistence_error(error, rid.0))?;
    if dispatch {
        if matches!(
            intent.operation.action,
            NativeAction::InstallImage | NativeAction::ImportImage
        ) {
            let tracker = if intent.operation.action == NativeAction::InstallImage {
                &state.image_installs
            } else {
                &state.oci_imports
            };
            if tracker.begin(&intent.definition().alias).is_err() {
                let _ = state.store.fail_image(&intent);
                return Err(unavailable(rid.0));
            }
        }
        let worker = dispatch_work(state.clone(), intent.clone(), guard);
        if matches!(
            intent.operation.action,
            NativeAction::DeleteImage | NativeAction::UpdateImageKernel
        ) {
            let _ = worker.await;
            return state
                .store
                .image_intent(id)
                .map_err(|error| persistence_error(error, rid.0))?
                .ok_or_else(|| AppError::internal(rid.0));
        }
    }
    Ok(intent)
}
fn checksum(path: &std::path::Path) -> std::io::Result<String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("Not a regular package"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
async fn import_auth(
    state: &AppState,
    registry: &str,
    rid: RequestId,
) -> Result<
    (
        Option<crate::persistence::iac_images::ImageCredential>,
        Option<crate::persistence::iac_images::ImageAuthPin>,
    ),
    AppError,
> {
    use crate::persistence::iac_images::ImageCredential;
    use firecrab_stack::ManagedResource;
    if !crate::oci::is_docker_hub_registry(registry) {
        for auth in state.store.pipe_auths().map_err(|_| unavailable(rid.0))? {
            let connection = auth.connection();
            if connection.registry.as_deref() != Some(registry) {
                continue;
            }
            let target = ManagedResource {
                kind: "auth".into(),
                id: connection.provider.as_str().into(),
            };
            let expected = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(
                        &serde_json::to_value(&auth).map_err(|_| unavailable(rid.0))?
                    )
                    .map_err(|_| unavailable(rid.0))?
                )
            );
            let pin = state
                .store
                .image_auth_pin(&target)
                .map_err(|_| unavailable(rid.0))?
                .filter(|pin| pin.sha256 == expected)
                .ok_or_else(|| unavailable(rid.0))?;
            let credential = auth
                .registry_credential()
                .await
                .map_err(|_| unavailable(rid.0))?;
            return Ok((
                Some(ImageCredential {
                    registry: credential.registry,
                    username: credential.username,
                    secret: credential.secret,
                }),
                Some(pin),
            ));
        }
    }
    let target = ManagedResource {
        kind: "registry".into(),
        id: if crate::oci::is_docker_hub_registry(registry) {
            "docker.io".into()
        } else {
            registry.into()
        },
    };
    let pin = state
        .store
        .image_auth_pin(&target)
        .map_err(|_| unavailable(rid.0))?;
    let credential = state
        .store
        .registry_credential(registry)
        .map_err(|_| unavailable(rid.0))?;
    match (credential, pin) {
        (Some(login), Some(pin)) => {
            let expected = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&serde_json::json!([login.username, login.secret]))
                        .map_err(|_| unavailable(rid.0))?
                )
            );
            if expected != pin.sha256 {
                return Err(unavailable(rid.0));
            }
            Ok((
                Some(ImageCredential {
                    registry: login.registry,
                    username: login.username,
                    secret: login.secret,
                }),
                Some(pin),
            ))
        }
        (None, None) => Ok((None, None)),
        _ => Err(unavailable(rid.0)),
    }
}
fn dispatch_work(state: AppState, intent: ImageIntent, guard: File) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let original = intent.clone();
        let store = state.store.clone();
        let tracker = if intent.operation.action == NativeAction::ImportImage {
            state.oci_imports.clone()
        } else {
            state.image_installs.clone()
        };
        let alias = intent.definition().alias.clone();
        let worker = tokio::spawn(async move { execute(&state, intent, guard).await });
        if !matches!(worker.await, Ok(Ok(()))) {
            let _ = store.fail_image(&original);
            tracker.finish_err_with(&alias, "Inspect and recover the original image operation");
        } else {
            tracker.finish_ok(&alias);
        }
    })
}
async fn execute(state: &AppState, intent: ImageIntent, guard: File) -> Result<(), ()> {
    let work = ImageWork::new(&state.templates, intent.clone(), guard).map_err(|_| ())?;
    let templates = (*state.templates)
        .clone()
        .with_image_authority(ImageAuthority::Operation(std::sync::Arc::new(work.clone())))
        .map_err(|_| ())?;
    let alias = &intent.definition().alias;
    match &intent.definition().source {
        ImageSource::Package { spec, .. } => {
            if let Some(prepared) = state.store.prepared_image(&intent).map_err(|_| ())? {
                templates.register_spec(prepared.spec).map_err(|_| ())?;
            } else {
                image_install::install_staged_package_once(&state.image_installs, &templates, spec)
                    .await
                    .map_err(|_| ())?;
            }
        }
        ImageSource::Kernel { spec, version, .. } => {
            let _lease = crate::kernel_lifecycle::borrow_existing(&templates, version)
                .await
                .map_err(|_| ())?;
            templates.register_spec(spec.clone()).map_err(|_| ())?;
        }
        ImageSource::Delete {
            artifacts,
            retain_artifacts,
            ..
        } => {
            if state
                .store
                .deploy_image_is_referenced(alias)
                .map_err(|_| ())?
            {
                return Err(());
            }
            work.effects().map_err(|_| ())?;
            templates.retire_image(alias).map_err(|_| ())?;
            if !retain_artifacts {
                for artifact in artifacts {
                    work.check().map_err(|_| ())?;
                    if state
                        .store
                        .deploy_image_is_referenced(alias)
                        .map_err(|_| ())?
                    {
                        return Err(());
                    }
                    templates
                        .remove_image_artifact(&artifact.relative, &artifact.sha256)
                        .map_err(|_| ())?;
                }
            }
        }
        ImageSource::Oci {
            naming_reference,
            credential,
            ..
        } => {
            if let Some(prepared) = state.store.prepared_image(&intent).map_err(|_| ())? {
                let _lease = retain_original_kernel(&templates, &work).await?;
                templates.register_spec(prepared.spec).map_err(|_| ())?;
            } else {
                let mut spec = crate::oci::retained_import_spec(&intent).map_err(|_| ())?;
                if std::fs::symlink_metadata(templates.image_root_path().join(&spec.rootfs)).is_ok()
                {
                    let _lease = retain_original_kernel(&templates, &work).await?;
                    let original = work.original_kernel().map_err(|_| ())?.ok_or(())?;
                    spec.kernel = original.relative;
                    spec.boot_args = original.boot_args;
                    templates.register_spec(spec).map_err(|_| ())?;
                } else {
                    let reference =
                        crate::oci::ImageReference::parse(naming_reference).map_err(|_| ())?;
                    let login = credential
                        .as_ref()
                        .map(|login| crate::oci::RegistryCredential {
                            registry: login.registry.clone(),
                            username: login.username.clone(),
                            secret: login.secret.clone(),
                        });
                    crate::oci::import_oci_image(
                        &state.oci_imports,
                        &templates,
                        &reference,
                        alias,
                        login.as_ref(),
                        None,
                    )
                    .await
                    .map_err(|_| ())?;
                }
            }
        }
    }
    work.check().map_err(|_| ())?;
    let installed = intent.operation.action != NativeAction::DeleteImage;
    let image = if installed {
        Some(images::installed_response_for_state(
            state,
            templates.resolve_alias(alias).ok_or(())?.as_ref(),
        ))
    } else {
        None
    };
    state
        .store
        .finish_image(
            &intent,
            &ImageOutcome {
                alias: alias.clone(),
                installed,
                image,
            },
        )
        .map_err(|_| ())
}
async fn retain_original_kernel(
    templates: &TemplateRegistry,
    work: &ImageWork,
) -> Result<Option<crate::kernel_lifecycle::KernelArtifactLease>, ()> {
    let original = work.original_kernel().map_err(|_| ())?.ok_or(())?;
    let ImageSource::Oci {
        kernel_version,
        kernel_override,
        ..
    } = &work.intent().definition().source
    else {
        return Err(());
    };
    let lease = if kernel_override.is_none() {
        Some(
            crate::kernel_lifecycle::borrow_existing(templates, kernel_version)
                .await
                .map_err(|_| ())?,
        )
    } else {
        None
    };
    let actual = crate::oci::kernel::inspect_kernel(
        &templates.image_root_path().join(&original.relative),
        Architecture::HOST,
    )
    .await
    .map_err(|_| ())?;
    work.kernel(&original.relative, actual.encoded(), &original.boot_args)
        .map_err(|_| ())?;
    Ok(lease)
}
pub(crate) async fn recover(
    state: AppState,
    rid: RequestId,
    id: Uuid,
    generation: u64,
) -> Result<NativeOperation, AppError> {
    tokio::spawn(async move {
        let original = state
            .store
            .image_intent(id)
            .map_err(|error| persistence_error(error, rid.0))?
            .ok_or_else(|| AppError::not_found(rid.0))?;
        original
            .definition()
            .root
            .check(state.templates.image_root_path())
            .map_err(|_| unavailable(rid.0))?;
        let guard = image_lifecycle::lock(&state.templates).map_err(|_| unavailable(rid.0))?;
        let (intent, dispatch) = state
            .store
            .recover_image_at(id, generation)
            .map_err(|error| persistence_error(error, rid.0))?;
        if dispatch {
            let _ = dispatch_work(state.clone(), intent, guard).await;
        }
        state
            .store
            .iac_native_operation(id)
            .map_err(|error| persistence_error(error, rid.0))?
            .ok_or_else(|| AppError::internal(rid.0))
    })
    .await
    .map_err(|_| AppError::internal(rid.0))?
}

#[cfg(test)]
#[path = "image_mutation_tests.rs"]
mod tests;
