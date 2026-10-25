//! Immutable boot inputs. Paths, credentials and environment values are excluded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Exact installed template content, independent of a mutable alias.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TemplatePin {
    /// Registered version, resolved without following the current alias.
    pub version: String,
    /// Lowercase SHA256 of the kernel bytes.
    pub kernel_sha256: String,
    /// Lowercase SHA256 of the root filesystem bytes.
    pub rootfs_sha256: String,
    /// Lowercase SHA256 of the optional initrd bytes; null requires no initrd.
    pub initrd_sha256: Option<String>,
    /// Lowercase SHA256 of the registered boot arguments.
    pub boot_args_sha256: String,
}

impl TemplatePin {
    pub(crate) fn valid(&self) -> bool {
        !self.version.is_empty()
            && self.version.len() <= 256
            && self
                .version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:+".contains(&b))
            && [
                &self.kernel_sha256,
                &self.rootfs_sha256,
                &self.boot_args_sha256,
            ]
            .into_iter()
            .chain(self.initrd_sha256.as_ref())
            .all(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    }
}

/// One verified registry version, including whether the alias currently selects it.
#[derive(Debug, Clone)]
pub struct TemplateSnapshot {
    /// Registered alias.
    pub name: String,
    /// Exact artifact identities.
    pub pin: TemplatePin,
    /// Select this version for legacy declarations without an explicit pin.
    pub current: bool,
}

/// Redacted identities covered by the plan fingerprint and retained after apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanInputs {
    /// Logical VM key to the exact installed boot template.
    #[serde(default)]
    pub templates: BTreeMap<String, TemplatePin>,
    /// Logical service key to the declared OCI digest, without registry credentials.
    #[serde(default)]
    pub service_digests: BTreeMap<String, String>,
}
