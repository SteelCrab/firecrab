//! Filesystem result contract shared by the native and WSL repair backends.
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct CheckResult {
    pub check_exit: i32,
    pub repair_exit: Option<i32>,
    pub verify_exit: i32,
    pub changed: bool,
    #[serde(default)]
    pub restored_files: u64,
}

impl CheckResult {
    pub fn verified(&self) -> bool {
        // A read-only check can abort on directory corruption with 4 | 8 (12).
        let checked = (self.check_exit == 0 && self.repair_exit.is_none())
            || (((1..=7).contains(&self.check_exit) || self.check_exit == 12)
                && self
                    .repair_exit
                    .is_some_and(|status| (0..=3).contains(&status)));
        checked
            && self.verify_exit == 0
            && self.changed == (self.repair_exit.is_some() || self.restored_files > 0)
    }
}
