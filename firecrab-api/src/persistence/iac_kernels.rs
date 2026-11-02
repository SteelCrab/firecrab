//! Kernel filesystem work keeps its original version, source checksum and private root scope.
use super::{
    PersistenceError, Store,
    database_services::{decode, encode},
    iac_native, iac_owners,
};
use firecrab_stack::{ManagedResource, NativeAction, NativeOperation, NativeOperationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Kernel work is separate from transient in-memory download progress.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS iac_kernel_operations (
 operation_id TEXT PRIMARY KEY REFERENCES iac_native_operations(operation_id),
 request_hash TEXT NOT NULL, definition TEXT NOT NULL, source_pin TEXT,
 effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN (0,1)), outcome TEXT
) STRICT;";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Only an observed image-root directory may scope kernel mutation.
pub(crate) struct KernelRootPin {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl KernelRootPin {
    /// Capture the physical installation before admission, without creating a directory.
    pub(crate) fn capture(path: &Path) -> std::io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::other("image root is not a directory"));
        }
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    /// Bind the identity to an already-open physical directory, then check its mapping.
    pub(crate) fn from_directory(path: &Path, directory: &std::fs::File) -> std::io::Result<Self> {
        let metadata = directory.metadata()?;
        if !metadata.is_dir() {
            return Err(std::io::Error::other("image root is not a directory"));
        }
        let pin = Self {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        pin.check(path)?;
        Ok(pin)
    }
    /// The current configured path must still name the original physical directory.
    pub(crate) fn check(&self, current: &Path) -> std::io::Result<()> {
        if Self::capture(current)? != *self {
            return Err(std::io::Error::other("original image root changed"));
        }
        Ok(())
    }
    /// Workers use the original private path after validating the current mapping.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Operator selectors are hashed before resolving a mutable `stable` alias.
pub(crate) enum KernelRequest {
    Install(String),
    Delete(String),
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Immutable target; credentials and arbitrary build commands are absent.
pub(crate) struct KernelDefinition {
    pub version: String,
    pub architecture: String,
    pub root: KernelRootPin,
    pub releases_enabled: bool,
    pub expected_image_sha256: Option<String>,
    pub recipe: String,
    pub initial_source: Option<KernelSourcePin>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Freeze the source checksum once, before downloading or compiling its archive.
pub(crate) struct KernelSourcePin {
    pub url: String,
    pub sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
/// Public installation receipt contains hashes and identity, without host paths.
pub(crate) struct KernelOutcome {
    pub version: String,
    pub architecture: String,
    pub installed: bool,
    pub image_sha256: Option<String>,
    pub source_sha256: Option<String>,
}
#[derive(Clone)]
/// The private journal is the only source of authority for a kernel worker.
pub(crate) struct KernelIntent {
    pub operation: NativeOperation,
    definition: KernelDefinition,
    request_hash: String,
}
impl KernelIntent {
    /// The frozen target is returned by reference so a worker cannot replace it.
    pub(crate) fn definition(&self) -> &KernelDefinition {
        &self.definition
    }
    /// Deletion and installation have distinct immutable operation actions.
    pub(crate) fn deleting(&self) -> bool {
        self.operation.action == NativeAction::DeleteKernel
    }
}
pub(crate) fn resource(architecture: &str, version: &str) -> ManagedResource {
    ManagedResource {
        kind: "kernel".into(),
        id: format!("{architecture}:{version}"),
    }
}
/// Host-local kernel selectors are exact immutable architecture/version identities.
pub(crate) fn validate_identity(target: &ManagedResource) -> Result<(), PersistenceError> {
    let (arch, version) = target
        .id
        .split_once(':')
        .ok_or(PersistenceError::InvalidStackResource)?;
    if target.kind != "kernel"
        || arch != crate::image_install::Architecture::HOST.as_str()
        || crate::kernel_manager::cache_path(version).is_none()
        || target.id != resource(arch, version).id
    {
        return Err(PersistenceError::InvalidStackResource);
    }
    Ok(())
}
fn hash<T: Serialize>(value: &T) -> Result<String, PersistenceError> {
    Ok(format!("{:x}", Sha256::digest(encode(value)?.as_bytes())))
}
fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|v| v.is_ascii_hexdigit())
}
fn read(conn: &Connection, id: Uuid) -> Result<Option<KernelIntent>, PersistenceError> {
    let Some(operation) = iac_native::read(conn, id)? else {
        return Ok(None);
    };
    if !matches!(
        operation.action,
        NativeAction::InstallKernel | NativeAction::DeleteKernel
    ) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    let (request_hash, raw): (String, String) = conn.query_row(
        "SELECT request_hash,definition FROM iac_kernel_operations WHERE operation_id=?1",
        [id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let definition: KernelDefinition = decode(&raw)?;
    if operation.ownership.resource != resource(&definition.architecture, &definition.version) {
        return Err(PersistenceError::IacOperationIdentity);
    }
    Ok(Some(KernelIntent {
        operation,
        definition,
        request_hash,
    }))
}
fn current(conn: &Connection, intent: &KernelIntent) -> Result<(), PersistenceError> {
    if intent.operation.state != NativeOperationState::Running
        || iac_native::read(conn, intent.operation.operation_id)?.as_ref()
            != Some(&intent.operation)
        || iac_owners::read(conn, &intent.operation.ownership.resource)?
            != intent.operation.ownership
    {
        return Err(PersistenceError::StaleIacOwner);
    }
    let saved =
        read(conn, intent.operation.operation_id)?.ok_or(PersistenceError::IacOperationIdentity)?;
    if saved.definition != intent.definition || saved.request_hash != intent.request_hash {
        return Err(PersistenceError::IacOperationIdentity);
    }
    super::iac_dependencies::validate(conn, intent.operation.operation_id)
}
impl Store {
    /// Observe the original immutable kernel intent without authorizing more filesystem work.
    pub(crate) fn kernel_intent(&self, id: Uuid) -> Result<Option<KernelIntent>, PersistenceError> {
        read(&self.lock(), id)
    }
    /// Exact raw selectors also recover progress for a previously resolved stable alias.
    pub(crate) fn latest_kernel(
        &self,
        request: &KernelRequest,
    ) -> Result<Option<KernelIntent>, PersistenceError> {
        let conn = self.lock();
        let id:Option<String>=conn.query_row("SELECT operation_id FROM iac_kernel_operations WHERE request_hash=?1 ORDER BY rowid DESC LIMIT 1",[hash(request)?],|r|r.get(0)).optional()?;
        id.map(|id| {
            read(
                &conn,
                Uuid::parse_str(&id).map_err(|_| PersistenceError::IacOperationIdentity)?,
            )
        })
        .transpose()
        .map(Option::flatten)
    }
    /// Retained VM pins protect older image versions that are no longer the current alias.
    pub(crate) fn kernel_in_use(&self, sha: &str) -> Result<bool, PersistenceError> {
        Ok(self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM vms WHERE template_kernel_sha256=?1)",
            [sha],
            |r| r.get(0),
        )?)
    }

    /// Borrowing verified bytes does not grant configuration authority or replay unfinished work.
    pub(crate) fn kernel_read_allowed(
        &self,
        target: &ManagedResource,
    ) -> Result<(), PersistenceError> {
        let busy: bool = self.lock().query_row(
            "SELECT EXISTS(SELECT 1 FROM iac_native_operations WHERE kind=?1 AND resource_id=?2 AND state IN ('running','needs_recovery'))",
            params![target.kind,target.id],|row|row.get(0))?;
        if busy {
            return Err(PersistenceError::IacResourceBusy);
        }
        Ok(())
    }
    /// Resolve original retries before reading a mutable alias, root mapping or download catalog.
    pub(crate) fn kernel_retry(
        &self,
        id: Uuid,
        request: &KernelRequest,
    ) -> Result<Option<KernelIntent>, PersistenceError> {
        let conn = self.lock();
        let Some(intent) = read(&conn, id)? else {
            return Ok(None);
        };
        if intent.request_hash != hash(request)? {
            return Err(PersistenceError::IacOperationIdentity);
        }
        Ok(Some(intent))
    }
    /// Save the entire installation identity and parent lease before filesystem dispatch.
    pub(crate) fn begin_kernel(
        &self,
        id: Uuid,
        request: KernelRequest,
        definition: KernelDefinition,
    ) -> Result<(KernelIntent, bool), PersistenceError> {
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
        if definition.architecture != crate::image_install::Architecture::HOST.as_str()
            || crate::kernel_manager::cache_path(&definition.version).is_none()
            || definition
                .expected_image_sha256
                .as_ref()
                .is_some_and(|v| !digest(v))
            || definition.recipe != crate::oci::kernel::BUILD_REVISION
            || definition.initial_source.as_ref().is_some_and(|pin| {
                !digest(&pin.sha256)
                    || crate::oci::kernel::source_url(&definition.version).as_deref()
                        != Some(pin.url.as_str())
            })
            || match &request {
                KernelRequest::Install(requested) => {
                    requested != "stable" && requested != &definition.version
                }
                KernelRequest::Delete(version) => version != &definition.version,
            }
        {
            return Err(PersistenceError::InvalidStackResource);
        }
        let target = resource(&definition.architecture, &definition.version);
        if !iac_owners::public_write_allowed(&tx, &target)? {
            return Err(PersistenceError::IacOwnershipConflict);
        }
        let operation = NativeOperation {
            operation_id: id,
            ownership: iac_owners::read(&tx, &target)?,
            action: match request {
                KernelRequest::Install(_) => NativeAction::InstallKernel,
                KernelRequest::Delete(_) => NativeAction::DeleteKernel,
            },
            state: NativeOperationState::Running,
            error_code: None,
            execution_generation: 0,
            configuration_generation: None,
            retain_disks: None,
        };
        iac_native::save(&tx, &operation)?;
        super::iac_dependencies::reserve(
            &tx,
            &operation,
            ManagedResource {
                kind: "host_configuration".into(),
                id: "default".into(),
            },
        )?;
        tx.execute("INSERT INTO iac_kernel_operations(operation_id,request_hash,definition,source_pin) VALUES(?1,?2,?3,?4)",params![id.to_string(),request_hash,encode(&definition)?,definition.initial_source.as_ref().map(encode).transpose()?])?;
        tx.commit()?;
        Ok((
            KernelIntent {
                operation,
                definition,
                request_hash,
            },
            true,
        ))
    }
    /// A stale compile cannot publish into a later attempt or another physical root.
    pub(crate) fn check_kernel(&self, intent: &KernelIntent) -> Result<(), PersistenceError> {
        current(&self.lock(), intent)
    }
    /// Persist the exact source once; an upstream checksum change cannot silently replace it.
    pub(crate) fn prepare_kernel_source(
        &self,
        intent: &KernelIntent,
        source: &KernelSourcePin,
    ) -> Result<(), PersistenceError> {
        if !digest(&source.sha256)
            || crate::oci::kernel::source_url(&intent.definition.version).as_deref()
                != Some(source.url.as_str())
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let old: Option<String> = tx.query_row(
            "SELECT source_pin FROM iac_kernel_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        if let Some(old) = old {
            if decode::<KernelSourcePin>(&old)? != *source {
                return Err(PersistenceError::IacOperationIdentity);
            }
        } else {
            tx.execute(
                "UPDATE iac_kernel_operations SET source_pin=?1 WHERE operation_id=?2",
                params![encode(source)?, intent.operation.operation_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Recovery reads its frozen source instead of resolving an upstream checksum again.
    pub(crate) fn kernel_source(
        &self,
        intent: &KernelIntent,
    ) -> Result<Option<KernelSourcePin>, PersistenceError> {
        let conn = self.lock();
        current(&conn, intent)?;
        let raw: Option<String> = conn.query_row(
            "SELECT source_pin FROM iac_kernel_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        raw.as_deref().map(decode).transpose()
    }
    /// Record uncertainty before any owned artifact removal, source download or compilation.
    pub(crate) fn kernel_effects(&self, intent: &KernelIntent) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        tx.execute(
            "UPDATE iac_kernel_operations SET effects_started=1 WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// A verified no-effect failure releases its lease; uncertain artifacts remain reserved.
    pub(crate) fn fail_kernel(&self, intent: &KernelIntent) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        let effects: bool = tx.query_row(
            "SELECT effects_started FROM iac_kernel_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        let mut operation = intent.operation.clone();
        operation.state = if effects {
            NativeOperationState::NeedsRecovery
        } else {
            NativeOperationState::Failed
        };
        operation.error_code = Some(
            if effects {
                "kernel_incomplete"
            } else {
                "kernel_unstarted"
            }
            .into(),
        );
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Only the exact observed uncertain attempt can resume the original target.
    pub(crate) fn recover_kernel_at(
        &self,
        id: Uuid,
        expected_generation: u64,
    ) -> Result<(KernelIntent, bool), PersistenceError> {
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
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(PersistenceError::IacOperationIdentity)?;
        intent.operation.state = NativeOperationState::Running;
        intent.operation.error_code = None;
        iac_native::save(&tx, &intent.operation)?;
        current(&tx, &intent)?;
        tx.commit()?;
        Ok((intent, true))
    }
    /// Hash/readiness evidence and the terminal acknowledgement commit together.
    pub(crate) fn finish_kernel(
        &self,
        intent: &KernelIntent,
        outcome: &KernelOutcome,
    ) -> Result<(), PersistenceError> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, intent)?;
        if outcome.version != intent.definition.version
            || outcome.architecture != intent.definition.architecture
            || outcome.installed == intent.deleting()
            || (outcome.installed && outcome.image_sha256.as_ref().is_none_or(|v| !digest(v)))
            || (outcome.installed
                && intent
                    .definition
                    .expected_image_sha256
                    .as_ref()
                    .is_some_and(|hash| outcome.image_sha256.as_ref() != Some(hash)))
        {
            return Err(PersistenceError::IacOperationIdentity);
        }
        let raw: Option<String> = tx.query_row(
            "SELECT source_pin FROM iac_kernel_operations WHERE operation_id=?1",
            [intent.operation.operation_id.to_string()],
            |r| r.get(0),
        )?;
        if let Some(raw) = raw {
            let source: KernelSourcePin = decode(&raw)?;
            if outcome.installed && outcome.source_sha256.as_ref() != Some(&source.sha256) {
                return Err(PersistenceError::IacOperationIdentity);
            }
        }
        let mut operation = intent.operation.clone();
        operation.state = NativeOperationState::Succeeded;
        operation.error_code = None;
        tx.execute(
            "UPDATE iac_kernel_operations SET outcome=?1 WHERE operation_id=?2",
            params![encode(outcome)?, operation.operation_id.to_string()],
        )?;
        iac_native::save(&tx, &operation)?;
        tx.commit()?;
        Ok(())
    }
    /// Acknowledgement remains immutable after later deletion or ownership changes.
    pub(crate) fn kernel_receipt(
        &self,
        id: Uuid,
    ) -> Result<Option<KernelOutcome>, PersistenceError> {
        let raw: Option<String> = self
            .lock()
            .query_row(
                "SELECT outcome FROM iac_kernel_operations WHERE operation_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        raw.as_deref().map(decode).transpose()
    }
}

#[cfg(test)]
#[path = "iac_kernel_tests.rs"]
mod tests;
