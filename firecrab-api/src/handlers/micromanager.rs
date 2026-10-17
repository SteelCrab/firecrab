//! The host controller fails closed if this endpoint is absent or unreachable.
use crate::state::AppState;
use axum::{Json, extract::State};
use firecrab_api_types::VmState;
use serde::Serialize;

#[derive(Serialize)]
pub struct Activity {
    busy: bool,
}

pub async fn activity(State(state): State<AppState>) -> Json<Activity> {
    // A process may still be alive while its record is being reconciled.
    let processes = !state
        .processes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty();
    let vms = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .any(|vm| {
            matches!(
                vm.state,
                VmState::Starting | VmState::Running | VmState::Stopping
            )
        });
    let jobs = [
        &state.image_installs,
        &state.image_packages,
        &state.kernel_installs,
        &state.oci_imports,
        &state.microregistry_registers,
    ]
    .iter()
    .any(|tracker| tracker.any_running());
    Json(Activity {
        busy: processes || vms || jobs || state.bootstraps.any_active() || updater_busy(),
    })
}

// The detached updater survives API restarts, so in-memory tracking is insufficient.
fn updater_busy() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return true;
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        match std::fs::read(entry.path().join("cmdline")) {
            Ok(command) if is_updater(&command) => return true,
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return true,
            _ => {}
        }
    }
    false
}

fn is_updater(command: &[u8]) -> bool {
    let args: Vec<_> = command
        .split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .collect();
    args.first()
        .is_some_and(|exe| exe.rsplit(|b| *b == b'/').next() == Some(b"firecrab".as_slice()))
        && args
            .windows(2)
            .any(|pair| pair == [b"update".as_slice(), b"--apply".as_slice()])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_updater_is_distinguished_from_release_polling() {
        assert!(is_updater(
            b"/usr/local/bin/firecrab\0update\0--apply\0--json\0"
        ));
        assert!(!is_updater(b"firecrab\0update\0--check\0"));
        assert!(!is_updater(b"bash\0-c\0firecrab update --apply\0"));
    }
    #[tokio::test]
    async fn detached_jobs_and_bootstrap_sessions_block_sleep() {
        let root = tempfile::tempdir().unwrap();
        let templates =
            crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty())
                .unwrap();
        let state = AppState::with_db_file(templates, root.path().join("state.db"))
            .await
            .unwrap();
        for tracker in [
            &state.image_installs,
            &state.image_packages,
            &state.kernel_installs,
            &state.oci_imports,
            &state.microregistry_registers,
        ] {
            tracker.begin("sleepy-qa").unwrap();
            assert!(activity(State(state.clone())).await.0.busy);
            tracker.finish_ok("sleepy-qa");
        }
        let session = state
            .bootstraps
            .try_begin("sleepy-qa", "alpine", uuid::Uuid::new_v4())
            .unwrap();
        assert!(activity(State(state.clone())).await.0.busy);
        state.bootstraps.finish_ok(session);
    }
}
