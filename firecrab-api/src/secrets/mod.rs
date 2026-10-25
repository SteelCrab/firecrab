//! PipeSecrets stores values and provider credentials in one encrypted host-local vault.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) mod providers;

/// Vault format marker, also authenticated as additional AES-GCM data.
const MAGIC: &[u8] = b"PIPESECRETS\x01";
/// Maximum encrypted file size, including metadata and transfer snapshots.
const MAX_VAULT_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum number of local secrets or values in one provider snapshot.
pub(crate) const MAX_SECRETS: usize = 1000;

/// Storage, validation, and provider failures that can be safely mapped to API errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SecretError {
    /// The vault cannot be read, decoded, or durably written.
    #[error("PipeSecrets storage is unavailable")]
    Storage,
    /// The key is missing or invalid, or the ciphertext fails authentication.
    #[error("PipeSecrets encryption key is missing or invalid")]
    Encryption,
    /// The requested secret, connection, or transfer preview no longer exists.
    #[error("record not found")]
    NotFound,
    /// Another record already owns the requested name.
    #[error("a record with this name already exists")]
    Duplicate,
    /// A captured record identity, version, or remote value has changed.
    #[error("the values changed; preview the transfer again")]
    Conflict,
    /// The transfer preview has exceeded its ten-minute lifetime.
    #[error("the transfer preview expired; preview the transfer again")]
    Expired,
    /// A static validation message that contains no supplied secret data.
    #[error("{0}")]
    Invalid(&'static str),
    /// An external request failed; its raw response must not reach the caller.
    #[error("external service request failed; check the connection and its permissions")]
    Provider,
}

impl From<std::io::Error> for SecretError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}

/// External store adapter selected by a saved connection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Provider {
    /// Another Firecrab API exposing the PipeSecrets endpoints.
    Firecrab,
    /// Regional AWS Secrets Manager using signed JSON requests.
    AwsSecretsManager,
}

impl Provider {
    /// Returns the identifier retained in import history after an adapter is removed.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Firecrab => "firecrab",
            Self::AwsSecretsManager => "awsSecretsManager",
        }
    }
}

/// Non-credential connection settings returned to the web console.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Settings {
    /// Firecrab API base URL; unused by the AWS adapter.
    #[serde(default)]
    pub base_url: String,
    /// AWS region used for the service endpoint and request signature.
    #[serde(default)]
    pub region: String,
    /// AWS name prefix used when listing secrets and validating uploads.
    #[serde(default)]
    pub prefix: String,
}

/// Public connection metadata; credentials remain in the encrypted record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionSummary {
    /// Stable local identifier used by transfer requests and source history.
    pub id: Uuid,
    /// Unique connection name shown in the web console.
    pub name: String,
    /// Adapter responsible for reading and writing the external store.
    pub provider: Provider,
    /// Provider scope and endpoint settings, without credentials.
    pub settings: Settings,
    /// Optimistic version incremented by each successful connection update.
    pub version: u64,
    /// Last successful update time in Unix milliseconds.
    pub updated_at: u64,
}

/// Encrypted connection record; intentionally omits Debug to protect credentials.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Connection {
    /// Metadata safe to include in connection API responses.
    pub summary: ConnectionSummary,
    /// Provider-specific authentication values, never returned to the console.
    pub credentials: BTreeMap<String, String>,
}

/// Secret metadata returned by list and save endpoints without the stored value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SecretSummary {
    /// Stable identifier retained when an import replaces an existing value.
    pub id: Uuid,
    /// Unique local name used to match external values during transfers.
    pub name: String,
    /// User description retained when an import replaces the secret.
    pub description: String,
    /// Optimistic version incremented by each successful edit or replacement.
    pub version: u64,
    /// Initial local creation time in Unix milliseconds.
    pub created_at: u64,
    /// Last local edit or import time in Unix milliseconds.
    pub updated_at: u64,
    /// Most recent import source, if the secret was imported.
    pub source: Option<SecretSource>,
}

/// Import provenance retained even after the external connection is removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SecretSource {
    /// Local identifier of the connection used for the import.
    pub connection_id: Uuid,
    /// Connection name captured at import time.
    pub connection_name: String,
    /// Historical provider identifier, including adapters that are no longer supported.
    pub provider: String,
    /// Secret name in the external store.
    pub remote_key: String,
    /// Successful import time in Unix milliseconds.
    pub imported_at: u64,
}

/// Encrypted secret record; the value is exposed only by explicit reveal requests.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Secret {
    /// Metadata safe to return without revealing the value.
    pub summary: SecretSummary,
    /// Stored UTF-8 value, which may be empty or contain multiple lines.
    pub value: String,
}

/// Direction of a manual, previewed transfer between local and external stores.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Direction {
    /// Copy captured external values into PipeSecrets.
    Import,
    /// Send selected PipeSecrets values to the external store.
    Export,
}

/// Effect of applying a captured value to its destination.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Change {
    /// The destination has no secret with this name.
    Add,
    /// The destination value differs and replacement needs explicit selection.
    Update,
    /// The destination already has the captured value; applying it is disabled.
    Unchanged,
}

/// Value-free preview row sent to the web console.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangeSummary {
    /// Secret name shared by the source and destination.
    pub name: String,
    /// Addition, replacement, or unchanged result for this name.
    pub change: Change,
}

/// Encrypted snapshot of one transfer value and the state required to apply it.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PlannedValue {
    /// Name and change type safe to display in a preview.
    pub summary: ChangeSummary,
    /// Captured source value to copy when the preview is applied.
    pub value: String,
    /// Captured local identity, or none when importing a new name.
    pub local_id: Option<Uuid>,
    /// Captured local version used to reject edits made after preview.
    pub local_version: Option<u64>,
    /// Captured remote value used to detect changes before an upload.
    pub remote_value: Option<String>,
}

/// Account credential binding captured by an import preview; contains no bearer token.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct AccountImport {
    /// Console account UUID that owns the captured values.
    pub account_id: Uuid,
    /// SHA-256 of the host credential; changing or disconnecting invalidates the preview.
    pub token_hash: String,
}

/// Encrypted, single-use transfer snapshot with a ten-minute lifetime.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Plan {
    /// Present only for imports from the linked console account.
    #[serde(default)]
    pub account: Option<AccountImport>,
    /// Identifier used to apply selected names from this preview.
    pub id: Uuid,
    /// Connection whose scope and credentials were used for the snapshot.
    pub connection_id: Uuid,
    /// Connection version that must still match when the preview is applied.
    pub connection_version: u64,
    /// Whether values move into PipeSecrets or into the external store.
    pub direction: Direction,
    /// Snapshot creation time in Unix milliseconds, used to enforce expiry.
    pub created_at: u64,
    /// Captured values and concurrency checks for each preview row.
    pub values: Vec<PlannedValue>,
}

/// Decrypted vault contents, held only while a locked read or write is performed.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Contents {
    /// Local secret values and metadata.
    pub secrets: Vec<Secret>,
    /// External connection settings and credentials.
    pub connections: Vec<Connection>,
    /// Temporary captured transfers awaiting application or expiry.
    pub plans: Vec<Plan>,
    /// Immutable Secret environments for deployments, never returned by the API.
    #[serde(default)]
    pub service_environments: BTreeMap<Uuid, BTreeMap<String, String>>,
    /// Version-pinned controller credentials, retained for durable cross-host retries.
    #[serde(default)]
    pub cluster_credentials: BTreeMap<String, ClusterCredentials>,
    /// Durable rotation admission/retry state; never exposed through Secrets APIs.
    #[serde(default)]
    pub service_rotations: BTreeMap<Uuid, ServiceRotation>,
}

/// Private credential snapshot; values and fingerprints never enter public journals.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ClusterCredentials {
    pub references: BTreeMap<String, firecrab_api_types::ClusterServiceCredential>,
    pub values: BTreeMap<String, String>,
}

/// Private reconciliation state. The value fingerprint remains encrypted in the vault.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServiceRotation {
    /// Successful source release fenced by the automatic update request.
    pub release_id: Uuid,
    /// Reused after a lost admission acknowledgement or a retryable admission error.
    pub operation_id: Uuid,
    /// Canonical value fingerprint; encrypted and excluded from public metadata.
    pub fingerprint: String,
    /// Number of admission attempts for these private values and this source release.
    pub attempts: u8,
    /// Earliest retry in Unix milliseconds, retained across API restarts.
    pub retry_after: u64,
    /// Exhausted retries wait for a different value or an explicit Deploy.
    pub paused: bool,
}

/// Encrypted file store opened lazily by Secrets requests.
///
/// File locks also serialize a second API process using the same data directory.
#[derive(Debug)]
pub(crate) struct Vault {
    /// Owner-only directory containing the key, ciphertext, and lock file.
    root: PathBuf,
}

impl Vault {
    /// Records the vault directory without creating files or reading its key.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Avoid creating an unused vault during periodic service reconciliation.
    pub(crate) fn is_initialized(&self) -> bool {
        self.root.join("vault.bin").is_file()
    }

    /// Runs a read operation on decrypted contents under an exclusive file lock.
    pub(crate) fn read<T>(
        &self,
        operation: impl FnOnce(&Contents) -> Result<T, SecretError>,
    ) -> Result<T, SecretError> {
        let (_lock, key) = self.open()?;
        operation(&self.load(&key)?)
    }

    /// Commits a successful operation with a fresh nonce and atomic file replacement.
    ///
    /// Failed operations leave the previous ciphertext unchanged.
    pub(crate) fn write<T>(
        &self,
        operation: impl FnOnce(&mut Contents) -> Result<T, SecretError>,
    ) -> Result<T, SecretError> {
        let (_lock, key) = self.open()?;
        let mut contents = self.load(&key)?;
        let result = operation(&mut contents)?;
        contents
            .plans
            .retain(|plan| now_ms().saturating_sub(plan.created_at) < 600_000);
        let bytes = serde_json::to_vec(&contents).map_err(|_| SecretError::Storage)?;
        if bytes.len() as u64 > MAX_VAULT_BYTES - 64 {
            return Err(SecretError::Invalid("PipeSecrets storage limit reached"));
        }
        let mut nonce = [0; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| SecretError::Encryption)?;
        let mut encrypted = bytes;
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(MAGIC),
            &mut encrypted,
        )
        .map_err(|_| SecretError::Encryption)?;
        let mut output = MAGIC.to_vec();
        output.extend_from_slice(&nonce);
        output.extend(encrypted);
        atomic_write(&self.root.join("vault.bin"), &output)?;
        Ok(result)
    }

    /// Acquires the vault lock and loads its key, creating a key only for a new vault.
    fn open(&self) -> Result<(File, LessSafeKey), SecretError> {
        fs::create_dir_all(&self.root)?;
        if fs::symlink_metadata(&self.root)?.file_type().is_symlink() {
            return Err(SecretError::Storage);
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        let lock = private_file()
            .create(true)
            .truncate(false)
            .open(self.root.join("vault.lock"))?;
        lock.lock()?;
        let key_path = self.root.join("master.key");
        let key_bytes = match private_file().open(&key_path) {
            Ok(mut file) => {
                fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
                let mut bytes = Vec::new();
                Read::by_ref(&mut file).take(33).read_to_end(&mut bytes)?;
                bytes
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.root.join("vault.bin").exists() {
                    return Err(SecretError::Encryption);
                }
                let mut bytes = vec![0; 32];
                SystemRandom::new()
                    .fill(&mut bytes)
                    .map_err(|_| SecretError::Encryption)?;
                atomic_write(&key_path, &bytes)?;
                bytes
            }
            Err(_) => return Err(SecretError::Encryption),
        };
        if key_bytes.len() != 32 {
            return Err(SecretError::Encryption);
        }
        let key = LessSafeKey::new(
            UnboundKey::new(&aead::AES_256_GCM, &key_bytes).map_err(|_| SecretError::Encryption)?,
        );
        Ok((lock, key))
    }

    /// Reads a bounded file and authenticates its contents before decoding the vault.
    fn load(&self, key: &LessSafeKey) -> Result<Contents, SecretError> {
        let mut file = match private_file().open(self.root.join("vault.bin")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Contents::default());
            }
            Err(_) => return Err(SecretError::Storage),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_VAULT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_VAULT_BYTES
            || !bytes.starts_with(MAGIC)
            || bytes.len() < MAGIC.len() + 12 + 16
        {
            return Err(SecretError::Encryption);
        }
        let nonce: [u8; 12] = bytes[MAGIC.len()..MAGIC.len() + 12]
            .try_into()
            .map_err(|_| SecretError::Encryption)?;
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(MAGIC),
                &mut bytes[MAGIC.len() + 12..],
            )
            .map_err(|_| SecretError::Encryption)?;
        decode_contents(plain)
    }
}

/// Removes retired connections while preserving imported copies and their provenance.
fn decode_contents(plain: &[u8]) -> Result<Contents, SecretError> {
    let mut stored: serde_json::Value =
        serde_json::from_slice(plain).map_err(|_| SecretError::Storage)?;
    let connections = stored
        .get_mut("connections")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(SecretError::Storage)?;
    let mut retired = BTreeSet::new();
    for connection in connections.iter() {
        if connection["summary"]["provider"] == "infisical" {
            let id = connection["summary"]["id"]
                .as_str()
                .ok_or(SecretError::Storage)?;
            retired.insert(Uuid::parse_str(id).map_err(|_| SecretError::Storage)?);
        }
    }
    connections.retain_mut(|connection| {
        if connection["summary"]["provider"] == "infisical" {
            return false;
        }
        // Older clients saved these unused settings on every provider.
        if let Some(settings) = connection
            .get_mut("summary")
            .and_then(|summary| summary.get_mut("settings"))
            .and_then(serde_json::Value::as_object_mut)
        {
            for field in ["projectId", "environment", "secretPath"] {
                settings.remove(field);
            }
        }
        true
    });
    let mut contents: Contents =
        serde_json::from_value(stored).map_err(|_| SecretError::Storage)?;
    contents
        .plans
        .retain(|plan| !retired.contains(&plan.connection_id));
    Ok(contents)
}

/// Opens owner-only read/write files without following a final symlink.
fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    options
}

/// Synchronizes a private temporary file, renames it, then synchronizes its parent.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SecretError> {
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file().create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(path.parent().ok_or(SecretError::Storage)?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// Returns Unix milliseconds for record timestamps and preview expiry checks.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Accepts 1-512 ASCII characters from the shared provider name alphabet.
pub(crate) fn validate_name(name: &str) -> Result<(), SecretError> {
    if name.is_empty()
        || name.len() > 512
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./+=@-".contains(&byte))
    {
        return Err(SecretError::Invalid(
            "name must contain 1-512 letters, numbers or these characters: _ . / + = @ -",
        ));
    }
    Ok(())
}

/// Accepts empty or multiline UTF-8 values up to 16384 bytes, without NUL bytes.
pub(crate) fn validate_value(value: &str) -> Result<(), SecretError> {
    if value.len() > 16_384 || value.contains('\0') {
        return Err(SecretError::Invalid(
            "value must be at most 16384 bytes without NUL",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
