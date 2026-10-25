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

/// Maximum concurrent scheduler workers, including those retrying result persistence.
const MAX_CONCURRENT: usize = 4;

/// RFC 3339 offsets are mandatory; cron has minute precision and named weekdays.
/// Return the first occurrence strictly after Unix millisecond `after_ms`, or
/// `None` if none remains. Daylight-saving gaps are skipped and repeated local
/// minutes use their earlier UTC instant only.
///
/// # Errors
/// Reject malformed timestamps, non-five-field cron, unknown IANA zones, and
/// recurring schedule search timestamps outside the supported date range.
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
/// Recover interrupted claims, then poll once per second with bounded concurrency.
/// The task retains the database ownership lock for its lifetime; acquiring that
/// lock before host recovery is the caller's responsibility.
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

/// Execute an already committed claim through the existing VM lifecycle handler.
/// Missing targets disable future runs. Retry only result persistence on database
/// failures, retaining the claim and worker slot without repeating VM side effects.
async fn execute(state: AppState, schedule: ScheduleResponse, run: ScheduleRun) {
    let purpose = state
        .vms
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&schedule.spec.vm_id)
        .map(|vm| vm.purpose);
    let error = if purpose.is_none() {
        Some("target VM no longer exists; schedule disabled".to_owned())
    } else if purpose == Some(crate::model::VmPurpose::Pool) {
        Some("pool members are managed automatically; schedule disabled".to_owned())
    } else if purpose == Some(crate::model::VmPurpose::Queue) {
        Some("Queue engines are managed through Queue controls; schedule disabled".to_owned())
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
        let target_unavailable = state
            .vms
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&schedule.spec.vm_id)
            .is_none_or(|vm| {
                matches!(
                    vm.purpose,
                    crate::model::VmPurpose::Pool | crate::model::VmPurpose::Queue
                )
            });
        let store = state.store.clone();
        let run = run.clone();
        let error = error.clone();
        let saved = tokio::task::spawn_blocking(move || {
            store.finish_schedule_run(
                &run,
                finished_at_ms,
                error,
                target_unavailable,
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
