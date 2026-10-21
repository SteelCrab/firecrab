//! PipeSecrets stores values and provider credentials in one encrypted host-local vault.

use std::collections::BTreeMap;
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

const MAGIC: &[u8] = b"PIPESECRETS\x01";
const MAX_VAULT_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_SECRETS: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub(crate) enum SecretError {
    #[error("PipeSecrets storage is unavailable")]
    Storage,
    #[error("PipeSecrets encryption key is missing or invalid")]
    Encryption,
    #[error("record not found")]
    NotFound,
    #[error("a record with this name already exists")]
    Duplicate,
    #[error("the values changed; preview the transfer again")]
    Conflict,
    #[error("the transfer preview expired; preview the transfer again")]
    Expired,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("external service request failed; check the connection and its permissions")]
    Provider,
}

impl From<std::io::Error> for SecretError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Provider {
    Firecrab,
    Infisical,
    AwsSecretsManager,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Settings {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub secret_path: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionSummary {
    pub id: Uuid,
    pub name: String,
    pub provider: Provider,
    pub settings: Settings,
    pub version: u64,
    pub updated_at: u64,
}

// No Debug implementation: credentials and values must not enter logs.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Connection {
    pub summary: ConnectionSummary,
    pub credentials: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SecretSummary {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub version: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub source: Option<SecretSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SecretSource {
    pub connection_id: Uuid,
    pub connection_name: String,
    pub provider: Provider,
    pub remote_key: String,
    pub imported_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Secret {
    pub summary: SecretSummary,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Direction {
    Import,
    Export,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Change {
    Add,
    Update,
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangeSummary {
    pub name: String,
    pub change: Change,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PlannedValue {
    pub summary: ChangeSummary,
    pub value: String,
    pub local_id: Option<Uuid>,
    pub local_version: Option<u64>,
    pub remote_value: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub connection_version: u64,
    pub direction: Direction,
    pub created_at: u64,
    pub values: Vec<PlannedValue>,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Contents {
    pub secrets: Vec<Secret>,
    pub connections: Vec<Connection>,
    pub plans: Vec<Plan>,
}

/// File locks also serialize a second API process using the same data directory.
#[derive(Debug)]
pub(crate) struct Vault {
    root: PathBuf,
}

impl Vault {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn read<T>(
        &self,
        operation: impl FnOnce(&Contents) -> Result<T, SecretError>,
    ) -> Result<T, SecretError> {
        let (_lock, key) = self.open()?;
        operation(&self.load(&key)?)
    }

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
        serde_json::from_slice(plain).map_err(|_| SecretError::Storage)
    }
}

fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    options
}

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

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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
