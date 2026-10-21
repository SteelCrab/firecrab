//! Editable host policies in the installed manifest, independent of release identity.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::manifest::{Error, Manifest};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Runtime {
    pub auto_restart: bool,
    pub ssh: Ssh,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            auto_restart: true,
            ssh: Ssh::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ssh {
    pub connect_timeout_secs: u32,
    pub keepalive_interval_secs: u32,
    pub keepalive_count_max: u32,
    pub reconnect_attempts: u32,
    pub reconnect_delay_secs: u32,
    pub restart_vm_on_failure: bool,
    pub reuse_connections: bool,
    pub persist_secs: u32,
}

impl Default for Ssh {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 10,
            keepalive_interval_secs: 15,
            keepalive_count_max: 3,
            reconnect_attempts: 5,
            reconnect_delay_secs: 5,
            restart_vm_on_failure: true,
            reuse_connections: true,
            persist_secs: 60,
        }
    }
}

impl Runtime {
    pub fn load(home: &Path) -> Result<Self, Error> {
        Ok(Manifest::installed(home)?
            .and_then(|manifest| manifest.runtime)
            .unwrap_or_default())
    }

    pub fn validate(&self) -> Result<(), Error> {
        for (name, value, min, max) in [
            (
                "connect_timeout_secs",
                self.ssh.connect_timeout_secs,
                1,
                300,
            ),
            (
                "keepalive_interval_secs",
                self.ssh.keepalive_interval_secs,
                0,
                3600,
            ),
            ("keepalive_count_max", self.ssh.keepalive_count_max, 1, 100),
            ("reconnect_attempts", self.ssh.reconnect_attempts, 0, 1000),
            (
                "reconnect_delay_secs",
                self.ssh.reconnect_delay_secs,
                1,
                300,
            ),
            ("persist_secs", self.ssh.persist_secs, 0, 86400),
        ] {
            if !(min..=max).contains(&value) {
                return Err(Error::Invalid(format!(
                    "runtime.ssh.{name} must be between {min} and {max}"
                )));
            }
        }
        Ok(())
    }
}
