//! Per-MicroVM skill targets and their sync into every coding CLI's user skill folder.
//! The host keeps the target (enabled names); the VM keeps a record of what it last received.
use super::*;
use crate::micromanager::skills::store::Store;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::BTreeSet;

const RECORD: &str = "/root/.local/share/firecrab/skills.json";

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Targets {
    #[serde(default)]
    enabled: BTreeSet<String>,
}

/// The last transfer's outcome, so a matching VM record still reports its conflicts.
#[derive(Serialize, Deserialize)]
struct Synced {
    record: Value,
    conflicts: Vec<String>,
}

fn targets(dir: &Path) -> Result<Targets, Error> {
    Ok(resource::read(&dir.join("skills.json"))?.unwrap_or_default())
}

fn vm_dir(home: &Path, vm: Uuid) -> PathBuf {
    home.join("runtime/agents/microvms").join(vm.to_string())
}

/// A stopped or unreachable VM keeps its target and reports `pending` instead of failing.
fn connect_and_sync(home: &Path, vm: Uuid, saved: &Targets) -> Result<Value, Error> {
    match Runtime::for_vm(home, vm, true) {
        Ok(runtime) => runtime.sync_skills(),
        Err(error) => Ok(json!({
            "enabled": saved.enabled,
            "state": "pending",
            "reason": error.to_string(),
            "clis": [],
            "conflicts": [],
        })),
    }
}

pub fn vm_skills(home: &Path, vm: Uuid) -> Result<Value, Error> {
    connect_and_sync(home, vm, &targets(&vm_dir(home, vm))?)
}

pub fn set_vm_skill(home: &Path, vm: Uuid, name: &str, enabled: bool) -> Result<Value, Error> {
    if enabled && !Store::new(home).imported()?.iter().any(|i| i.name == name) {
        return Err(invalid("Import this skill from the store first."));
    }
    let dir = vm_dir(home, vm);
    let mut saved = targets(&dir)?;
    if enabled {
        saved.enabled.insert(name.to_owned());
    } else {
        saved.enabled.remove(name);
    }
    resource::write(&dir.join("skills.json"), &saved)?;
    connect_and_sync(home, vm, &saved)
}

impl Runtime {
    /// Bring the VM's CLI skill folders to the saved target. Transfer errors become `failed`.
    pub(super) fn sync_skills(&self) -> Result<Value, Error> {
        let guest = self
            .guest
            .as_ref()
            .ok_or_else(|| invalid("Select a MicroVM first."))?;
        let saved = targets(&self.home)?;
        let store = Store::new(&self.skills_home);
        let wanted: Vec<_> = store
            .imported()?
            .into_iter()
            .filter(|i| saved.enabled.contains(&i.name))
            .collect();
        let record = json!({"skills": wanted
            .iter()
            .map(|i| json!({"name": i.name, "digest": i.digest}))
            .collect::<Vec<_>>()});
        let clis: Vec<_> = DRIVERS
            .iter()
            .filter(|d| d.0 != "github")
            .map(|(id, name, command)| {
                json!({"id": id, "name": name, "installed": guest.available.contains(*command)})
            })
            .collect();
        let mut value = json!({
            "enabled": saved.enabled,
            "state": "applied",
            "reason": null,
            "clis": clis,
            "conflicts": [],
        });
        let fail = |mut value: Value, error: Error| {
            value["state"] = json!("failed");
            value["reason"] = json!(error.to_string());
            Ok(value)
        };
        let current = match guest.shell(
            &format!("if [ -f {RECORD} ]; then head -c 65536 {RECORD} | base64 -w0; fi"),
            Duration::from_secs(10),
        ) {
            Ok(current) => current,
            Err(error) => return fail(value, error),
        };
        let installed: Option<Value> = STANDARD
            .decode(current.trim())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let last = self.home.join("skills-synced.json");
        if installed.as_ref() == Some(&record) {
            if let Some(synced) = resource::read::<Synced>(&last)?
                && synced.record == record
            {
                value["conflicts"] = json!(synced.conflicts);
            }
            return Ok(value);
        }
        let names: Vec<String> = wanted.iter().map(|i| i.name.clone()).collect();
        let script = format!(
            "/bin/sh -c {} -- {}",
            guest::quote(include_str!("skills_sync.sh")),
            guest::quote(&STANDARD.encode(record.to_string()))
        );
        match guest.shell_with_input(&script, &store.archive(&names)?, Duration::from_secs(120)) {
            Ok(output) => {
                let conflicts: Vec<String> = output
                    .lines()
                    .filter_map(|l| l.strip_prefix("conflict:").map(str::to_owned))
                    .collect();
                resource::write(
                    &last,
                    &Synced {
                        record,
                        conflicts: conflicts.clone(),
                    },
                )?;
                value["conflicts"] = json!(conflicts);
                Ok(value)
            }
            Err(error) => fail(value, error),
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
