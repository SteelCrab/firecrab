//! Native image operations retain one alias slot and immutable boot versions.
use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use crate::{image_install::Architecture, templates::TemplateSpec};
use firecrab_stack::{
    ManagedResource, NativeAction, NativeOperation, NativeOperationState, TemplatePin,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use uuid::Uuid;

/// Private intents and catalog epochs; creator triggers require the pool schema first.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_image_availability (
 alias TEXT PRIMARY KEY, retired INTEGER NOT NULL CHECK(retired IN (0,1)), revision INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TRIGGER IF NOT EXISTS iac_image_vm_insert BEFORE INSERT ON vms
 WHEN EXISTS(SELECT 1 FROM iac_image_availability WHERE alias=NEW.template AND retired=1)
 OR EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='image' AND resource_id=NEW.template AND state IN ('running','needs_recovery'))
 BEGIN SELECT RAISE(ABORT,'iac_image_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS iac_image_pool_insert BEFORE INSERT ON vm_pools
 WHEN EXISTS(SELECT 1 FROM iac_image_availability WHERE alias=NEW.template AND retired=1)
 OR EXISTS(SELECT 1 FROM iac_native_operations WHERE kind='image' AND resource_id=NEW.template AND state IN ('running','needs_recovery'))
 BEGIN SELECT RAISE(ABORT,'iac_image_unavailable'); END;
CREATE TABLE IF NOT EXISTS iac_image_operations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 request_hash TEXT NOT NULL, definition TEXT NOT NULL,
 prepared_image TEXT, prepared_kernel TEXT, effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN (0,1)),
 outcome TEXT
) STRICT;";

/// Raw selectors recover their original receipt before reading a mutable catalog.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum ImageRequest {
    /// Install from the already staged package, without downloading a different source.
    Install {
        /// Original alias selector used to recognize retries before catalog lookup.
        alias: String,
    },
    /// Pair the original installed rootfs with a selected managed kernel.
    Kernel {
        /// Original image management slot.
        alias: String,
        /// Requested managed release, retained separately from the resulting image version.
        version: String,
    },
    /// Remove registration with the caller's original artifact retention policy.
    Delete {
        /// Alias whose complete retained history belongs to this operation.
        alias: String,
        /// Keep artifact bytes while retiring their registration.
        retain_artifacts: bool,
    },
    /// Materialize an OCI reference after freezing its resolved manifest.
    Import {
        /// Original naming selector; it never substitutes for the frozen pull digest.
        reference: String,
    },
}
impl ImageRequest {
    /// Map the original selector to its public durable action identity.
    fn action(&self) -> NativeAction {
        match self {
            Self::Install { .. } => NativeAction::InstallImage,
            Self::Kernel { .. } => NativeAction::UpdateImageKernel,
            Self::Delete { .. } => NativeAction::DeleteImage,
            Self::Import { .. } => NativeAction::ImportImage,
        }
    }
}
/// Private locators accompany a public immutable boot identity.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageVersion {
    /// Exact private locators associated with this prepared output.
    pub spec: TemplateSpec,
    /// Complete boot identity that recovery must preserve.
    pub pin: TemplatePin,
}
/// The selected kernel becomes immutable before a nondeterministic rootfs is published.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageKernelPin {
    /// Verified image-root-relative kernel location.
    pub relative: PathBuf,
    /// Original kernel bytes; rebuilding different bytes cannot repair this operation.
    pub sha256: String,
    /// Original boot arguments, independent of defaults in a later API binary.
    pub boot_args: String,
}
/// Legacy metadata can survive external artifact removal without an invented boot hash.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ObservedImageVersion {
    /// Original catalog locators even when one or more files are missing.
    pub spec: TemplateSpec,
    /// Absent only for legacy metadata that never recorded its boot hashes.
    pub pin: Option<TemplatePin>,
}
/// Credentials are private journal input, never an observation or a Debug field.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageCredential {
    /// Exact registry hostname; credentials must never follow another registry.
    pub registry: String,
    /// Original registry account name.
    pub username: String,
    /// Private password or token; never returned in operation observations.
    pub secret: String,
}
/// Authentication ownership and configuration must still match when source work is admitted.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ImageAuthPin {
    /// Original source resource, owner and ownership generation.
    pub ownership: firecrab_stack::ResourceOwnership,
    /// Canonical private configuration digest, without copying secrets into public state.
    pub sha256: String,
}
/// An original removal candidate still needs a live-reference check before unlink.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageArtifact {
    /// Original deletion candidate beneath the retained image root.
    pub relative: PathBuf,
    /// Expected file contents, rechecked before unlinking.
    pub sha256: String,
}
/// Private action inputs; a callback cannot replace these after admission.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ImageSource {
    /// A staged package identified by its original archive hash.
    Package {
        /// Original package locators and desired boot arguments.
        spec: TemplateSpec,
        /// Archive bytes checked through the same opened extraction handle.
        archive_sha256: String,
    },
    /// An existing rootfs paired with an explicitly selected kernel.
    Kernel {
        /// Resulting immutable version and original rootfs/initrd locators.
        spec: TemplateSpec,
        /// Managed kernel release whose artifact is borrowed through registration.
        version: String,
        /// Complete expected output, including unchanged rootfs and boot arguments.
        pin: TemplatePin,
    },
    /// Original removal scope; recovery never replaces it with newly observed files.
    Delete {
        /// All original versions, including unready legacy registrations.
        versions: Vec<ObservedImageVersion>,
        /// Exact file candidates; current references still take precedence over deletion.
        artifacts: Vec<ImageArtifact>,
        /// The original retention decision is part of request identity.
        retain_artifacts: bool,
    },
    /// Original OCI source and authority, separate from the prepared ext4 output.
    Oci {
        /// Digest-qualified pull reference, never the mutable display tag.
        reference: String,
        /// Original reference used to derive the public alias and version.
        naming_reference: String,
        /// Original tag or digest shown by the installed image.
        display_version: String,
        /// Selected managed release, or the operator-kernel selector.
        kernel_version: String,
        /// Content-addressed operator copy and hash, when an override was selected.
        kernel_override: Option<(PathBuf, String)>,
        /// Original private registry credential; absent for anonymous pulls.
        credential: Option<ImageCredential>,
        /// Source ownership/configuration snapshot reserved until terminal resolution.
        authentication: Option<ImageAuthPin>,
    },
}
/// Physical namespace, original alias observation and dependency scope for one operation.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageDefinition {
    /// Stable management slot shared by the alias and its retained versions.
    pub alias: String,
    /// Original host CPU architecture; recovery cannot move to another architecture.
    pub architecture: String,
    /// Physical directory identity checked before filesystem effects.
    pub root: super::iac_kernels::KernelRootPin,
    /// Original selected metadata; missing files do not imply missing registration.
    pub before: Option<ObservedImageVersion>,
    /// Frozen action-specific source or retention inputs.
    pub source: ImageSource,
    /// Parent configuration slots reserved alongside the implicit host configuration slot.
    pub dependencies: Vec<ManagedResource>,
}
impl ImageDefinition {
    /// Return the originally selected output version, including the pre-delete version.
    pub(crate) fn version(&self) -> &str {
        match &self.source {
            ImageSource::Package { spec, .. } | ImageSource::Kernel { spec, .. } => &spec.version,
            ImageSource::Oci {
                display_version, ..
            } => display_version,
            ImageSource::Delete { .. } => self
                .before
                .as_ref()
                .map_or("", |image| image.spec.version.as_str()),
        }
    }
    /// Validate resolved private inputs against the original raw request and host architecture.
    fn matches(&self, request: &ImageRequest) -> bool {
        if self.architecture != Architecture::HOST.as_str() || !valid_alias(&self.alias) {
            return false;
        }
        match (request, &self.source) {
            (
                ImageRequest::Install { alias },
                ImageSource::Package {
                    spec,
                    archive_sha256,
                },
            ) => alias == &self.alias && spec.alias == self.alias && digest(archive_sha256),
            (
                ImageRequest::Kernel { alias, version },
                ImageSource::Kernel {
                    spec,
                    version: prepared,
                    pin,
                },
            ) => {
                alias == &self.alias
                    && version == prepared
                    && spec.alias == self.alias
                    && spec.version == pin.version
                    && valid_pin(pin)
                    && self.before.is_some()
            }
            (
                ImageRequest::Delete {
                    alias,
                    retain_artifacts,
                },
                ImageSource::Delete {
                    versions,
                    artifacts,
                    retain_artifacts: frozen,
                },
            ) => {
                alias == &self.alias
                    && retain_artifacts == frozen
                    && self.before.is_some()
                    && versions.iter().all(|image| {
                        image.spec.alias == self.alias
                            && image.pin.as_ref().is_none_or(|pin| {
                                image.spec.version == pin.version && valid_pin(pin)
                            })
                    })
                    && artifacts
                        .iter()
                        .all(|artifact| digest(&artifact.sha256) && relative(&artifact.relative))
            }
            (
                ImageRequest::Import { reference },
                ImageSource::Oci {
                    reference: pinned,
                    naming_reference,
                    display_version,
                    kernel_version,
                    kernel_override,
                    credential,
                    authentication,
                },
            ) => {
                let Ok(requested) = crate::oci::ImageReference::parse(reference) else {
                    return false;
                };
                let Ok(frozen) = crate::oci::ImageReference::parse(pinned) else {
                    return false;
                };
                naming_reference == reference
                    && frozen.version.is_immutable()
                    && credential.is_some() == authentication.is_some()
                    && authentication
                        .as_ref()
                        .is_none_or(|pin| digest(&pin.sha256))
                    && match kernel_override {
                        Some((path, sha)) => {
                            kernel_version == "operator"
                                && digest(sha)
                                && *path
                                    == PathBuf::from(".oci/kernel")
                                        .join(Architecture::HOST.as_str())
                                        .join(format!("custom-{sha}"))
                        }
                        None => crate::kernel_manager::cache_path(kernel_version).is_some(),
                    }
                    && requested.registry == frozen.registry
                    && requested.repository == frozen.repository
                    && (!requested.version.is_immutable() || requested.version == frozen.version)
                    && crate::oci::template_name_from_reference(&requested).is_ok_and(|name| {
                        name.alias == self.alias && name.version == *display_version
                    })
            }
            _ => false,
        }
    }
}
/// Terminal results stay independent of future alias selection and physical removal.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageOutcome {
    /// Exact alias acknowledged by the original completed request.
    pub alias: String,
    /// Original completion result, rather than current catalog readiness.
    pub installed: bool,
    /// Original installed-image response, retained for lost kernel-update acknowledgements.
    pub image: Option<firecrab_api_types::ImageResponse>,
}
/// Only persistence admission constructs an executable original image intent.
#[derive(Clone)]
pub(crate) struct ImageIntent {
    /// Exact ownership and execution-generation snapshot carried by the worker.
    pub operation: NativeOperation,
    /// Original private inputs; only persistence admission constructs an intent.
    definition: ImageDefinition,
    /// Original raw-request digest, separate from resolved private source details.
    request_hash: String,
}
impl ImageIntent {
    /// Borrow the original private definition without exposing it in public observations.
    pub(crate) fn definition(&self) -> &ImageDefinition {
        &self.definition
    }
}
/// Use one stable alias slot for all image versions and alias-selection changes.
pub(crate) fn resource(alias: &str) -> ManagedResource {
    ManagedResource {
        kind: "image".into(),
        id: alias.into(),
    }
}
/// Exclude internal aliases and path syntax from public image management identities.
pub(crate) fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 256
        && !alias.starts_with('.')
        && !alias.starts_with("__")
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}
/// Accept only nonempty, image-root-relative paths with ordinary components.
fn relative(path: &std::path::Path) -> bool {
    path.components().next().is_some()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}
/// Recognize the lowercase SHA-256 form stored in boot and source pins.
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
/// Require every recorded boot component hash to be a complete SHA-256 digest.
fn valid_pin(pin: &TemplatePin) -> bool {
    !pin.version.is_empty()
        && [
            &pin.kernel_sha256,
            &pin.rootfs_sha256,
            &pin.boot_args_sha256,
        ]
        .into_iter()
        .chain(pin.initrd_sha256.as_ref())
        .all(|value| digest(value))
}
/// Hash canonical private JSON for immutable retry and configuration comparisons.
fn hash<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?.as_bytes())))
}
/// Decode only image operations and verify that their saved alias matches their resource identity.
fn read(conn: &Connection, id: Uuid) -> Result<Option<ImageIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if !matches!(
        operation.action,
        NativeAction::InstallImage
            | NativeAction::UpdateImageKernel
            | NativeAction::DeleteImage
            | NativeAction::ImportImage
    ) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (request_hash, definition): (String, String) = conn.query_row(
        "SELECT request_hash,definition FROM iac_image_operations WHERE operation_id=?1",
        [id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let definition: ImageDefinition = decode(&definition)?;
    if operation.ownership.resource != resource(&definition.alias) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(ImageIntent {
        operation,
        definition,
        request_hash,
    }))
}
/// Fence stale execution, ownership, dependency and source-authentication snapshots.
fn current(conn: &Connection, intent: &ImageIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
        || iac_owners::read(conn, &intent.operation.ownership.resource)?
            != intent.operation.ownership
    {
        return Err(PersistenceError::StaleIacExecution);
    }
    let saved =
        read(conn, intent.operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if saved.request_hash != intent.request_hash
        || hash(&saved.definition)? != hash(&intent.definition)?
    {
        return Err(PersistenceError::IacOperationIdentity);
    }
    if let ImageSource::Oci {
        authentication: Some(expected),
        ..
    } = &intent.definition.source
        && auth_pin(conn, &expected.ownership.resource)?.as_ref() != Some(expected)
    {
        return Err(PersistenceError::StaleIacExecution);
    }
    super::iac_dependencies::validate(conn, intent.operation.operation_id)
}
/// Read the exact private provider or registry configuration and its current owner together.
fn auth_pin(
    conn: &Connection,
    target: &ManagedResource,
) -> Result<Option<ImageAuthPin>, PersistenceError> {
    let value: Option<serde_json::Value> = match (target.kind.as_str(), target.id.as_str()) {
        ("auth", _) => conn
            .query_row(
                "SELECT credential FROM pipe_auth_credentials WHERE provider=?1",
                [&target.id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|raw| decode(&raw))
            .transpose()?,
        ("registry", "docker.io" | "registry-1.docker.io") => conn
            .query_row(
                "SELECT username,secret FROM docker_hub_credential WHERE id=1",
                [],
                |row| {
                    Ok(serde_json::json!([
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?
                    ]))
                },
            )
            .optional()?,
        ("registry", _) => conn
            .query_row(
                "SELECT username,secret FROM registry_credentials WHERE registry=?1",
                [&target.id],
                |row| {
                    Ok(serde_json::json!([
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?
                    ]))
                },
            )
            .optional()?,
        _ => return Err(PersistenceError::InvalidStackResource),
    };
    value
        .map(|value| {
            Ok(ImageAuthPin {
                ownership: iac_owners::read(conn, target)?,
                sha256: hash(&value)?,
            })
        })
        .transpose()
}
impl Store {
    /// Freeze selected kernel bytes and boot arguments before rootfs publication; identical retries are harmless.
    pub(crate) fn prepare_image_kernel(
        &self,
        intent: &ImageIntent,
        pin: &ImageKernelPin,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if !matches!(intent.definition.source, ImageSource::Oci { .. })
            || !relative(&pin.relative)
            || !digest(&pin.sha256)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let saved: Option<String> = tx.query_row(
            "SELECT prepared_kernel FROM iac_image_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |row| row.get(0),
        )?;
        let encoded = encode(pin)?;
        if saved.is_some_and(|saved| saved != encoded) {
            return Err(PersistenceError::IacOperationIdentity);
        }
        tx.execute(
            "UPDATE iac_image_operations SET prepared_kernel=?1 WHERE operation_id=?2",
            params![encoded, intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Read original kernel preparation only for the currently authorized execution.
    pub(crate) fn prepared_image_kernel(
        &self,
        intent: &ImageIntent,
    ) -> Result<Option<ImageKernelPin>, PersistenceError> {
        let conn = self.lock();
        current(&conn, intent)?;
        let saved: Option<String> = conn.query_row(
            "SELECT prepared_kernel FROM iac_image_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |row| row.get(0),
        )?;
        saved.as_deref().map(decode).transpose()
    }
    /// A late creator that selected an Arc before deletion cannot resurrect the removed alias.
    pub(crate) fn record_image_availability(
        &self,
        alias: &str,
        retired: bool,
    ) -> Result<(), PersistenceError> {
        self.lock().execute(
            "INSERT INTO iac_image_availability(alias,retired,revision) VALUES(?1,?2,1)
            ON CONFLICT(alias) DO UPDATE SET retired=excluded.retired,revision=revision+1",
            params![alias, retired],
        )?;
        Ok(())
    }
    /// Capture a secret-free source fingerprint without granting mutation permission.
    pub(crate) fn image_auth_pin(
        &self,
        target: &ManagedResource,
    ) -> Result<Option<ImageAuthPin>, PersistenceError> {
        auth_pin(&self.lock(), target)
    }
    /// Observe the catalog epoch that invalidates previously captured asynchronous writers.
    pub(crate) fn image_revision(&self, alias: &str) -> Result<u64, PersistenceError> {
        image_revision(&self.lock(), alias)
    }
    /// Capture ownership and catalog epoch atomically, before any asynchronous source work.
    pub(crate) fn capture_image_writer(
        &self,
        alias: &str,
    ) -> Result<(firecrab_stack::ResourceOwnership, u64), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let target = resource(alias);
        if !iac_owners::public_write_allowed(&tx, &target)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let snapshot = (iac_owners::read(&tx, &target)?, image_revision(&tx, alias)?);
        tx.commit()?;
        Ok(snapshot)
    }
    /// Keep the exact observed authority through the filesystem catalog commit.
    pub(crate) fn with_image_writer<T>(
        &self,
        expected: &firecrab_stack::ResourceOwnership,
        expected_revision: u64,
        action: impl FnOnce() -> Result<T, crate::templates::TemplateError>,
    ) -> Result<T, crate::templates::TemplateError> {
        let mut conn = self.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| crate::templates::TemplateError::ImageMutationDenied)?;
        if iac_owners::read(&tx, &expected.resource)
            .map_err(|_| crate::templates::TemplateError::ImageMutationDenied)?
            != *expected
            || image_revision(&tx, &expected.resource.id)
                .map_err(|_| crate::templates::TemplateError::ImageMutationDenied)?
                != expected_revision
            || !iac_owners::public_write_allowed(&tx, &expected.resource)
                .map_err(|_| crate::templates::TemplateError::ImageMutationDenied)?
        {
            return Err(crate::templates::TemplateError::ImageMutationDenied);
        }
        let value = action()?;
        tx.commit()
            .map_err(|_| crate::templates::TemplateError::ImageMutationDenied)?;
        Ok(value)
    }
    /// Observe an original private intent without admitting new work or changing its inputs.
    pub(crate) fn image_intent(&self, id: Uuid) -> Result<Option<ImageIntent>, PersistenceError> {
        read(&self.lock(), id)
    }
    /// Resolve raw-request retries before reading mutable catalogs, files or authentication.
    pub(crate) fn image_retry(
        &self,
        id: Uuid,
        request: &ImageRequest,
    ) -> Result<Option<ImageIntent>, PersistenceError> {
        let conn = self.lock();
        let Some(intent) = read(&conn, id)? else {
            return Ok(None);
        };
        if intent.request_hash != hash(request)? || intent.operation.action != request.action() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        Ok(Some(intent))
    }
    /// Atomically reserve the alias and dependencies, preserving original retries and active fences.
    pub(crate) fn begin_image(
        &self,
        id: Uuid,
        request: ImageRequest,
        definition: ImageDefinition,
    ) -> Result<(ImageIntent, bool), PersistenceError> {
        if id.is_nil() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let request_hash = hash(&request)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(intent) = read(&tx, id)? {
            if intent.request_hash != request_hash {
                return Err(PersistenceError::IacOperationIdentity);
            }
            return Ok((intent, false));
        }
        if !definition.matches(&request) {
            return Err(PersistenceError::InvalidStackResource);
        }
        if let ImageSource::Oci {
            authentication: Some(expected),
            ..
        } = &definition.source
            && auth_pin(&tx, &expected.ownership.resource)?.as_ref() != Some(expected)
        {
            return Err(PersistenceError::StaleIacExecution);
        }
        let target = resource(&definition.alias);
        if !iac_owners::public_write_allowed(&tx, &target)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        if matches!(
            request,
            ImageRequest::Delete { .. } | ImageRequest::Kernel { .. }
        ) && super::deploy_images::referenced(&tx, &definition.alias)?
        {
            return Err(PersistenceError::IacResourceBusy);
        }
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &target)?,
            action: request.action(),
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        let mut dependencies = definition.dependencies.clone();
        if let ImageSource::Oci {
            authentication: Some(pin),
            ..
        } = &definition.source
        {
            dependencies.push(pin.ownership.resource.clone());
        }
        dependencies.push(ManagedResource {
            kind: "host_configuration".into(),
            id: "default".into(),
        });
        dependencies.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
        dependencies.dedup();
        for parent in dependencies {
            if parent != target {
                super::iac_dependencies::reserve(&tx, &operation, parent)?;
            }
        }
        tx.execute("INSERT INTO iac_image_operations(operation_id,request_hash,definition) VALUES(?1,?2,?3)",
            params![id.to_string(),request_hash,encode(&definition)?])?;
        tx.commit()?;
        Ok((
            ImageIntent {
                operation,
                definition,
                request_hash,
            },
            true,
        ))
    }
    /// Validate a sealed execution snapshot without changing operation state.
    pub(crate) fn check_image(&self, intent: &ImageIntent) -> Result<(), PersistenceError> {
        current(&self.lock(), intent)
    }
    /// Read the latest alias operation for a specific public action, including after restart.
    pub(crate) fn latest_image(
        &self,
        alias: &str,
        action: NativeAction,
    ) -> Result<Option<ImageIntent>, PersistenceError> {
        let conn = self.lock();
        let id: Option<String> = conn.query_row(
            "SELECT operation_id FROM iac_native_operations WHERE kind='image' AND resource_id=?1 AND json_extract(status,'$.action')=json_extract(?2,'$') ORDER BY rowid DESC LIMIT 1",
            params![alias,encode(&action)?], |row| row.get(0),
        ).optional()?;
        id.map(|id| {
            Uuid::parse_str(&id)
                .map_err(|_| PersistenceError::IacOperationIdentity)
                .and_then(|id| read(&conn, id))
        })
        .transpose()
        .map(Option::flatten)
    }
    /// A prepared nondeterministic rootfs cannot be replaced by a different rebuild.
    pub(crate) fn prepare_image(
        &self,
        intent: &ImageIntent,
        image: &ImageVersion,
    ) -> Result<(), PersistenceError> {
        if image.spec.alias != intent.definition.alias
            || image.spec.version != intent.definition.version()
            || image.pin.version != image.spec.version
            || !valid_pin(&image.pin)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if let ImageSource::Kernel { pin, .. } = &intent.definition.source
            && pin != &image.pin
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if matches!(intent.definition.source, ImageSource::Oci { .. }) {
            let raw: Option<String> = tx.query_row(
                "SELECT prepared_kernel FROM iac_image_operations WHERE operation_id=?1",
                [intent.operation.operation_id.to_string()],
                |row| row.get(0),
            )?;
            let kernel: ImageKernelPin = decode(
                raw.as_deref()
                    .ok_or(PersistenceError::IacOperationIdentity)?,
            )?;
            if kernel.relative != image.spec.kernel
                || kernel.sha256 != image.pin.kernel_sha256
                || kernel.boot_args != image.spec.boot_args
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
        }
        let saved: Option<String> = tx.query_row(
            "SELECT prepared_image FROM iac_image_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |row| row.get(0),
        )?;
        let encoded = encode(image)?;
        if saved.as_ref().is_some_and(|saved| saved != &encoded) {
            return Err(PersistenceError::IacOperationIdentity);
        }
        tx.execute(
            "UPDATE iac_image_operations SET prepared_image=?1 WHERE operation_id=?2",
            params![encoded, intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Read the exact prepared output; its paths and hashes cannot change on recovery.
    pub(crate) fn prepared_image(
        &self,
        intent: &ImageIntent,
    ) -> Result<Option<ImageVersion>, PersistenceError> {
        let conn = self.lock();
        current(&conn, intent)?;
        let saved: Option<String> = conn.query_row(
            "SELECT prepared_image FROM iac_image_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |row| row.get(0),
        )?;
        saved.as_deref().map(decode).transpose()
    }
    /// Record possible managed effects before publication or removal can become visible.
    pub(crate) fn image_effects(&self, intent: &ImageIntent) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        tx.execute(
            "UPDATE iac_image_operations SET effects_started=1 WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Release known unstarted work, while uncertain effects retain their original reservations.
    pub(crate) fn fail_image(&self, intent: &ImageIntent) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let effects: bool = tx.query_row(
            "SELECT effects_started FROM iac_image_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |row| row.get(0),
        )?;
        let mut operation = intent.operation.clone();
        operation.state = if effects {
            NativeOperationState::NeedsRecovery
        } else {
            NativeOperationState::Failed
        };
        operation.error_code = Some(
            if effects {
                "image_incomplete"
            } else {
                "image_unstarted"
            }
            .into(),
        );
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Advance only the observed uncertain execution generation; delayed recoveries cannot steal it.
    pub(crate) fn recover_image_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<(ImageIntent, bool), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut intent = read(&tx, id)?.ok_or(PersistenceError::IacOperationIdentity)?;
        if matches!(
            intent.operation.state,
            NativeOperationState::Succeeded | NativeOperationState::Failed
        ) {
            return Ok((intent, false));
        }
        iac_native::check_recovery_attempt(&intent.operation, expected_generation)?;
        if intent.operation.state != NativeOperationState::NeedsRecovery {
            return Err(PersistenceError::IacResourceBusy);
        }
        intent.operation.execution_generation = intent
            .operation
            .execution_generation
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        intent.operation.state = NativeOperationState::Running;
        intent.operation.error_code = None;
        iac_native::save(&tx, &intent.operation)?;
        current(&tx, &intent)?;
        tx.commit()?;
        Ok((intent, true))
    }
    /// Validate the original prepared result and atomically save its immutable terminal receipt.
    pub(crate) fn finish_image(
        &self,
        intent: &ImageIntent,
        outcome: &ImageOutcome,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if outcome.alias != intent.definition.alias
            || outcome.installed == (intent.operation.action == NativeAction::DeleteImage)
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        if outcome.installed {
            let raw: Option<String> = tx.query_row(
                "SELECT prepared_image FROM iac_image_operations WHERE operation_id=?1",
                [intent.operation.operation_id.to_string()],
                |row| row.get(0),
            )?;
            let prepared: ImageVersion = decode(
                raw.as_deref()
                    .ok_or(PersistenceError::IacOperationIdentity)?,
            )?;
            let image = outcome
                .image
                .as_ref()
                .ok_or(PersistenceError::IacOperationIdentity)?;
            if image.alias != outcome.alias
                || !image.installed
                || image.version != prepared.pin.version
                || image.kernel_sha256 != prepared.pin.kernel_sha256
                || image.rootfs_sha256 != prepared.pin.rootfs_sha256
                || image.initrd_sha256 != prepared.pin.initrd_sha256
            {
                return Err(PersistenceError::IacOperationIdentity);
            }
        } else if outcome.image.is_some() {
            return Err(PersistenceError::IacOperationIdentity);
        }
        tx.execute(
            "UPDATE iac_image_operations SET outcome=?1 WHERE operation_id=?2",
            params![encode(outcome)?, intent.operation.operation_id.to_string()],
        )?;
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        operation.error_code = None;
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Read an immutable acknowledgement independently of later alias, file or owner changes.
    pub(crate) fn image_receipt(&self, id: Uuid) -> Result<Option<ImageOutcome>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT outcome FROM iac_image_operations WHERE operation_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        raw.as_deref().map(decode).transpose()
    }
}

/// Read the persisted epoch with a legacy zero default and reject invalid counters.
fn image_revision(conn: &Connection, alias: &str) -> Result<u64, PersistenceError> {
    let revision: i64 = conn
        .query_row(
            "SELECT revision FROM iac_image_availability WHERE alias=?1",
            [alias],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    revision
        .try_into()
        .map_err(|_| PersistenceError::IacOperationIdentity)
}

#[cfg(test)]
#[path = "iac_image_tests.rs"]
mod tests;
