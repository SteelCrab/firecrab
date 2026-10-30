//! Durable CODE operations reuse VM APIs and a pinned guest transport.
pub(crate) mod guard;
mod guest;
mod native;
use crate::{
    persistence::{Store, code_runtime::Record},
    state::AppState,
};
use firecrab_api_types::{
    CodeRuntimeAction as Action, CodeRuntimeFailure as Failure, CodeRuntimePhase as Phase,
};
pub(crate) use native::{Native, request as native_request};
use std::{
    fs::File,
    future::Future,
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    time::Duration,
};
use uuid::Uuid;

/// VM side effects are isolated here so journal recovery can be tested without KVM.
pub(crate) trait Driver: Sync {
    fn create(&self, record: &Record) -> impl Future<Output = Result<(), Failure>> + Send;
    fn start(&self, record: &Record) -> impl Future<Output = Result<(), Failure>> + Send;
    fn prepare(
        &self,
        record: &Record,
    ) -> impl Future<Output = Result<(Uuid, String), Failure>> + Send;
    fn stop(&self, record: &Record) -> impl Future<Output = Result<(), Failure>> + Send;
}

pub(crate) fn directory(state: &AppState, id: Uuid) -> PathBuf {
    state
        .sandbox
        .root
        .join("code-runtimes")
        .join(id.to_string())
}

/// Locks survive for the entire worker, including HTTP cancellation, across API processes.
pub(crate) fn lock(state: &AppState, id: Uuid) -> io::Result<File> {
    let dir = directory(state, id);
    for path in [
        &state.sandbox.root,
        &state.sandbox.root.join("code-runtimes"),
        &dir,
    ] {
        std::fs::create_dir_all(path)?;
        if !std::fs::symlink_metadata(path)?.is_dir() {
            return Err(io::Error::other("Invalid control directory"));
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("operation.lock"))?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("Invalid operation lock"));
    }
    file.try_lock().map_err(io::Error::other)?;
    Ok(file)
}

async fn save(store: &Store, record: &Record) -> Result<(), Failure> {
    let store = store.clone();
    let copy = record.clone();
    if tokio::task::spawn_blocking(move || store.save_code_runtime(&copy))
        .await
        .ok()
        .and_then(Result::ok)
        != Some(true)
    {
        return Err(Failure::OwnershipChanged);
    }
    Ok(())
}

/// Re-entry inspects the reserved UUID; it never allocates another VM or resets a checkout.
pub(crate) async fn execute(store: &Store, mut record: Record, driver: &impl Driver) {
    if !record.runtime.phase.pending() {
        return;
    }
    let result = async {
        match record.runtime.action {
            Action::Start => {
                record.runtime.phase = Phase::Provisioning;
                save(store, &record).await?;
                driver.create(&record).await?;
                record.vm_created = true;
                save(store, &record).await?;
                driver.start(&record).await?;
                record.runtime.phase = Phase::Preparing;
                save(store, &record).await?;
                let (runtime, commit) = driver.prepare(&record).await?;
                record.runtime.runtime_id = Some(runtime);
                record.runtime.initial_commit = Some(commit);
                record.runtime.phase = Phase::Ready;
            }
            Action::Stop => {
                record.runtime.phase = Phase::Stopping;
                save(store, &record).await?;
                driver.stop(&record).await?;
                record.runtime.phase = Phase::Stopped;
            }
        }
        Ok::<_, Failure>(())
    }
    .await;
    if let Err(error) = result {
        record.runtime.phase = Phase::Failed;
        record.runtime.error = Some(error);
    }
    let _ = save(store, &record).await;
}

pub(crate) fn dispatch(state: AppState, workspace: Uuid, operation: Uuid) {
    let Ok(permit) = state.code_jobs.clone().try_acquire_owned() else {
        return;
    };
    tokio::spawn(async move {
        let _permit = permit;
        let Ok(_claim) = lock(&state, workspace) else {
            return;
        };
        let Ok(Some(record)) = state.store.code_runtime(workspace) else {
            return;
        };
        if record.runtime.operation_id != operation {
            return;
        }
        execute(
            &state.store,
            record,
            &Native {
                state: state.clone(),
            },
        )
        .await;
    });
}

/// Resume unfinished journal operations after the normal VM/network recovery completes.
pub(crate) fn start_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(10));
        loop {
            timer.tick().await;
            let store = state.store.clone();
            if let Ok(Ok(records)) =
                tokio::task::spawn_blocking(move || store.code_runtime_pending()).await
            {
                for record in records {
                    dispatch(
                        state.clone(),
                        record.runtime.workspace_id,
                        record.runtime.operation_id,
                    );
                }
            }
        }
    });
}
