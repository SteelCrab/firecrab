//! Durable claims precede side effects. Interrupted runs are never replayed.

use std::str::FromStr;
use std::time::Duration;

use axum::extract::{Extension, Path, State};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use firecrab_api_types::{ScheduleAction, ScheduleResponse, ScheduleRun, ScheduleTiming};
use tokio::task::JoinSet;

use crate::handlers::vms;
use crate::persistence::PersistenceError;
use crate::server::RequestId;
use crate::state::AppState;

const MAX_CONCURRENT: usize = 4;

/// RFC 3339 offsets are mandatory; cron has minute precision and named weekdays.
pub(crate) fn next_run(timing: &ScheduleTiming, after_ms: i64) -> Result<Option<i64>, String> {
    match timing {
        ScheduleTiming::Once { at } => {
            let at = DateTime::parse_from_rfc3339(at)
                .map_err(|_| "use an RFC 3339 timestamp with an explicit offset".to_owned())?
                .timestamp_millis();
            Ok((at > after_ms).then_some(at))
        }
        ScheduleTiming::Cron {
            expression,
            time_zone,
        } => {
            if expression.len() > 256 || expression.split_whitespace().count() != 5 {
                return Err("use five cron fields: minute hour day month weekday".to_owned());
            }
            let zone = Tz::from_str(time_zone)
                .map_err(|_| "use an IANA time zone, such as Asia/Seoul".to_owned())?;
            let cron = cron::Schedule::from_str(&format!("0 {expression}"))
                .map_err(|_| "invalid cron expression".to_owned())?;
            let after = DateTime::<Utc>::from_timestamp_millis(after_ms)
                .ok_or_else(|| "timestamp is out of range".to_owned())?;
            Ok(cron
                .after(&after.with_timezone(&zone))
                // Select the first offset of a repeated local minute only.
                // The cron iterator can yield an earlier UTC instant when
                // started inside the second copy of the DST hour.
                .find(|at| {
                    at.timestamp_millis() > after_ms
                        && zone
                            .from_local_datetime(&at.naive_local())
                            .earliest()
                            .is_some_and(|first| first == *at)
                })
                .map(|at| at.timestamp_millis()))
        }
    }
}

/// Start only after VM reconciliation and listener binding have succeeded.
pub(crate) async fn spawn(
    state: AppState,
    ownership: std::fs::File,
) -> Result<(), PersistenceError> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.recover_schedules(Utc::now().timestamp_millis()))
        .await
        .map_err(|error| PersistenceError::CorruptRecord {
            id: "scheduler".to_owned(),
            reason: error.to_string(),
        })??;
    tokio::spawn(async move {
        let _ownership = ownership;
        let mut jobs = JoinSet::new();
        let mut ticks = tokio::time::interval(Duration::from_secs(1));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                result = jobs.join_next(), if !jobs.is_empty() => {
                    if let Some(Err(error)) = result {
                        tracing::error!(%error, "scheduled operation panicked; its run remains claimed until restart");
                    }
                }
                _ = ticks.tick() => {
                    let available = MAX_CONCURRENT.saturating_sub(jobs.len());
                    if available == 0 { continue; }
                    let store = state.store.clone();
                    let claimed = tokio::task::spawn_blocking(move || {
                        store.claim_schedules(Utc::now().timestamp_millis(), available)
                    }).await;
                    match claimed {
                        Ok(Ok(runs)) => for (schedule, run) in runs {
                            jobs.spawn(execute(state.clone(), schedule, run));
                        },
                        result => tracing::error!(?result, "failed to claim scheduled operations"),
                    }
                }
            }
        }
    });
    Ok(())
}

async fn execute(state: AppState, schedule: ScheduleResponse, run: ScheduleRun) {
    let exists = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(&schedule.spec.vm_id);
    let error = if !exists {
        Some("target VM no longer exists; schedule disabled".to_owned())
    } else {
        let args = (
            State(state.clone()),
            Extension(RequestId(run.id)),
            Path(schedule.spec.vm_id.to_string()),
        );
        let result = match schedule.spec.action {
            ScheduleAction::Start => vms::start_vm(args.0, args.1, args.2).await,
            ScheduleAction::Stop => vms::stop_vm(args.0, args.1, args.2).await,
        };
        result.err().map(|error| error.description())
    };
    let finished_at_ms = Utc::now().timestamp_millis();
    // Keep the claim and worker slot until the result is durable. A database
    // outage must not make the same operation eligible to execute again.
    loop {
        let target_missing = !state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(&schedule.spec.vm_id);
        let store = state.store.clone();
        let run = run.clone();
        let error = error.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store.finish_schedule_run(
                &run,
                finished_at_ms,
                error,
                target_missing,
                Utc::now().timestamp_millis(),
            )
        })
        .await;
        match saved {
            Ok(Ok(())) => break,
            result => {
                tracing::error!(?result, schedule_id = %schedule.id, "failed to persist scheduled operation result")
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests;
