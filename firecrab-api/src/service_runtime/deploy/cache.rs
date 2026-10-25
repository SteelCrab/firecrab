//! Retain current, in-progress and rollback images; collect only owned rootfs.

use super::*;

pub(crate) fn schedule(state: AppState) {
    tokio::spawn(async move {
        if let Err(error) = collect(&state).await {
            tracing::warn!(%error, "Deploy image cache cleanup will retry");
        }
    });
}

pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = collect(&state).await {
                tracing::warn!(%error, "Deploy image cache cleanup will retry");
            }
        }
    });
}

pub(super) async fn collect(state: &AppState) -> Result<usize, String> {
    let _claim = state.service_mutations.lock().await;
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        if state.oci_imports.any_running() || state.image_installs.any_running() {
            return Ok(0);
        }
        let mut removed = 0;
        let candidates = state
            .store
            .deploy_images()
            .map_err(|error| error.to_string())?;
        for spec in candidates {
            let deleted = state
                .templates
                .prune_deploy_image(&spec, || {
                    if state.oci_imports.any_running() || state.image_installs.any_running() {
                        return Ok(false);
                    }
                    state
                        .store
                        .deploy_image_is_referenced(&spec.alias)
                        .map(|used| !used)
                        .map_err(std::io::Error::other)
                })
                .map_err(|error| error.to_string())?;
            if !deleted {
                continue;
            }
            state
                .store
                .forget_deploy_image(&spec.rootfs)
                .map_err(|error| error.to_string())?;
            state.oci_imports.clear_finished(&spec.alias);
            removed += 1;
        }
        Ok(removed)
    })
    .await
    .map_err(|_| "Deploy cache cleanup task failed".to_owned())?
}

/// A crash can leave the published, journaled file before alias registration.
pub(crate) async fn restore_cached(
    state: &AppState,
    alias: &str,
    digest: &str,
) -> Result<bool, String> {
    if state.oci_imports.is_running(alias) {
        return Ok(false);
    }
    let state = state.clone();
    let alias = alias.to_owned();
    let digest = digest.to_owned();
    tokio::task::spawn_blocking(move || {
        for spec in state
            .store
            .deploy_images()
            .map_err(|error| error.to_string())?
            .into_iter()
            .rev()
        {
            if spec.alias != alias || spec.version != digest {
                continue;
            }
            if !state
                .templates
                .image_root_path()
                .join(&spec.rootfs)
                .is_file()
            {
                continue;
            }
            state
                .templates
                .register_spec(spec)
                .map_err(|_| "Could not restore the journaled Deploy image")?;
            return Ok(true);
        }
        Ok(false)
    })
    .await
    .map_err(|_| "Deploy image recovery task failed".to_owned())?
}

#[cfg(test)]
mod tests;
