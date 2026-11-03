//! Periodic cleanup never expires uncertain credential effects or ordinary Secret data.

use crate::state::AppState;
use std::time::Duration;

pub(crate) fn start(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let store = state.store.clone();
            let vault = state.secrets.clone();
            match tokio::task::spawn_blocking(move || {
                store.prune_resource_plan_inputs(&vault, super::now_ms())
            })
            .await
            {
                Ok(Ok(count)) if count > 0 => tracing::info!(
                    count,
                    "Pruned expired or acknowledged private IaC input scopes"
                ),
                Ok(Ok(_)) => (),
                _ => tracing::warn!(
                    "Private IaC input cleanup is unavailable; recovery material retained"
                ),
            }
        }
    });
}
