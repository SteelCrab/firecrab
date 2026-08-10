//! In-process tracker for from-scratch distro bootstrap sessions
//! (`POST /api/images/{alias}/bootstrap` and friends) — same shape as the
//! image-install tracker, but for a session whose terminal action is
//! "package as a `.tar.zst`" for `image_install.rs` to pick up (see
//! `public-docs/images.md`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{
    BootstrapResponse, BootstrapStatus, BootstrapStep, BootstrapStepOutcome, BootstrapStepRun,
};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct BootstrapTracker {
    sessions: Arc<Mutex<HashMap<Uuid, BootstrapResponse>>>,
}

impl BootstrapTracker {
    /// Registers a new session in `Booting` and returns its id — but only
    /// while no other session is still active, with that check and the
    /// insertion under a single lock acquisition. An `any_active()` call
    /// followed by a separate insert is a TOCTOU window wide enough for two
    /// concurrent `POST /api/images/{alias}/bootstrap` requests (a dashboard
    /// double-click is enough) to both pass the gate and boot their own
    /// builder VM. Returns `None` when a session is already running, so the
    /// caller can refuse with `409` instead.
    pub fn try_begin(&self, alias: &str, source_alias: &str, vm_id: Uuid) -> Option<Uuid> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if sessions.values().any(is_active) {
            return None;
        }
        Some(insert_session(&mut sessions, alias, source_alias, vm_id))
    }

    pub fn get(&self, id: Uuid) -> Option<BootstrapResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
    }

    /// Whether any tracked session hasn't reached a terminal status — a
    /// cheap pre-check for `handlers::bootstrap::start_bootstrap`, which
    /// still has to go through [`try_begin`](Self::try_begin) for the
    /// authoritative, race-free version of the same question (only one
    /// bootstrap runs at a time; see the design doc's rationale —
    /// chroot/mount/mkfs on a shared build path).
    pub fn any_active(&self) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .any(is_active)
    }

    /// Compare-and-set: only advances a session still in `expected`,
    /// returning whether it applied — a detached watcher must never clobber
    /// a status a later request already moved past.
    pub fn set_status_from(
        &self,
        id: Uuid,
        expected: BootstrapStatus,
        next: BootstrapStatus,
    ) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&id) {
            Some(session) if session.status == expected => {
                session.status = next;
                true
            }
            _ => false,
        }
    }

    /// Advances the session's step timeline: closes whatever step was open
    /// as succeeded, then opens `step`. Unlike `set_status_from` this is
    /// unconditional — every call site sits immediately after the status
    /// transition it accompanies, so the compare-and-set has already
    /// decided whether this session is the one still moving.
    pub fn set_step(&self, id: Uuid, step: BootstrapStep) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&id) {
            open_step(session, now_ms(), step);
        }
    }

    pub fn append_log(&self, id: Uuid, line: impl AsRef<str>) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            session.log.push('\n');
            session.log.push_str(&format!(
                "[{}] {}",
                elapsed_label(session.started_at_ms, now_ms()),
                line.as_ref()
            ));
        }
    }

    pub fn finish_ok(&self, id: Uuid) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            session.status = BootstrapStatus::Succeeded;
            close_open_step(session, now_ms(), BootstrapStepOutcome::Succeeded, None);
            session.ended_at_ms = Some(now_ms());
        }
    }

    pub fn finish_err(&self, id: Uuid, reason: impl AsRef<str>) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&id) {
            session.status = BootstrapStatus::Failed;
            session.log.push('\n');
            session.log.push_str(&format!(
                "[{}] {}",
                elapsed_label(session.started_at_ms, now_ms()),
                reason.as_ref()
            ));
            close_open_step(
                session,
                now_ms(),
                BootstrapStepOutcome::Failed,
                Some(reason.as_ref()),
            );
            session.ended_at_ms = Some(now_ms());
        }
    }

    /// Compare-and-set variant of [`finish_err`](Self::finish_err), for the
    /// same reason [`set_status_from`](Self::set_status_from) is one.
    pub fn finish_err_from(
        &self,
        id: Uuid,
        expected: BootstrapStatus,
        reason: impl AsRef<str>,
    ) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&id) {
            Some(session) if session.status == expected => {
                session.status = BootstrapStatus::Failed;
                session.log.push('\n');
                session.log.push_str(&format!(
                    "[{}] {}",
                    elapsed_label(session.started_at_ms, now_ms()),
                    reason.as_ref()
                ));
                close_open_step(
                    session,
                    now_ms(),
                    BootstrapStepOutcome::Failed,
                    Some(reason.as_ref()),
                );
                session.ended_at_ms = Some(now_ms());
                true
            }
            _ => false,
        }
    }

    /// Drops a session from the tracker (cancel path — the caller is
    /// responsible for tearing down the builder VM itself first).
    pub fn remove(&self, id: Uuid) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
    }
}

/// Builds the opening snapshot for [`BootstrapTracker::try_begin`], split
/// out so the insertion stays readable inside the single lock acquisition
/// that makes the active-session check atomic.
fn insert_session(
    sessions: &mut HashMap<Uuid, BootstrapResponse>,
    alias: &str,
    source_alias: &str,
    vm_id: Uuid,
) -> Uuid {
    let id = Uuid::new_v4();
    let now = now_ms();
    sessions.insert(
        id,
        BootstrapResponse {
            bootstrap_id: id,
            alias: alias.to_owned(),
            source_alias: source_alias.to_owned(),
            vm_id,
            status: BootstrapStatus::Booting,
            current_step: Some(BootstrapStep::StartingBuilderVm),
            step_timeline: vec![BootstrapStepRun {
                step: BootstrapStep::StartingBuilderVm,
                started_at_ms: now,
                ended_at_ms: None,
                outcome: BootstrapStepOutcome::Running,
                detail: None,
            }],
            log: format!("[{}] builder VM starting", elapsed_label(now, now)),
            started_at_ms: now,
            ended_at_ms: None,
        },
    );
    id
}

/// A session still holding the single bootstrap slot.
fn is_active(session: &BootstrapResponse) -> bool {
    !matches!(
        session.status,
        BootstrapStatus::Succeeded | BootstrapStatus::Failed
    )
}

/// Closes whichever step is still open, if any. Idempotent, so both the
/// success and failure paths can call it unconditionally — same shape as
/// `handlers::vms::close_open_step`, which does this for VM startup.
fn close_open_step(
    session: &mut BootstrapResponse,
    now: u64,
    outcome: BootstrapStepOutcome,
    detail: Option<&str>,
) {
    if let Some(run) = session
        .step_timeline
        .iter_mut()
        .find(|run| run.outcome == BootstrapStepOutcome::Running)
    {
        run.ended_at_ms = Some(now);
        run.outcome = outcome;
        run.detail = detail.map(str::to_owned);
    }
    session.current_step = None;
}

/// Closes the open step as succeeded and opens `step` in its place.
fn open_step(session: &mut BootstrapResponse, now: u64, step: BootstrapStep) {
    close_open_step(session, now, BootstrapStepOutcome::Succeeded, None);
    session.step_timeline.push(BootstrapStepRun {
        step,
        started_at_ms: now,
        ended_at_ms: None,
        outcome: BootstrapStepOutcome::Running,
        detail: None,
    });
    session.current_step = Some(step);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Log-line stamp, relative to when this session started. The absolute
/// epoch second this used to print (`[1785900123s]`) carried no usable
/// information for a reader scanning a single session's log.
/// `saturating_sub` because these are wall-clock reads, not monotonic ones,
/// and an NTP step backwards must not wrap into a nonsense duration.
fn elapsed_label(started_at_ms: u64, now_ms: u64) -> String {
    format!("+{}s", now_ms.saturating_sub(started_at_ms) / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firecrab_api_types::{BootstrapStep, BootstrapStepOutcome};

    #[test]
    fn log_lines_are_stamped_relative_to_the_session_start() {
        assert_eq!(elapsed_label(1_000_000, 1_000_000), "+0s");
        assert_eq!(elapsed_label(1_000_000, 1_042_000), "+42s");
        // A clock that jumps backwards must not underflow into a huge number.
        assert_eq!(elapsed_label(1_042_000, 1_000_000), "+0s");
    }

    #[test]
    fn begin_then_snapshot_returns_a_booting_session() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");

        let snapshot = tracker.get(id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Booting);
        assert_eq!(snapshot.alias, "ubuntu-26.04");
        assert_eq!(snapshot.source_alias, "alpine-3.24");
    }

    #[test]
    fn get_returns_none_for_an_unknown_id() {
        let tracker = BootstrapTracker::default();
        assert!(tracker.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn any_active_is_true_only_while_a_session_is_non_terminal() {
        let tracker = BootstrapTracker::default();
        assert!(!tracker.any_active());

        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");
        assert!(tracker.any_active());

        tracker.finish_ok(id);
        assert!(!tracker.any_active());
    }

    /// The single-session gate the dashboard's double-click can otherwise
    /// slip through: `any_active()` + a separate insert leaves a window
    /// where both callers see "nothing active". `try_begin` closes it by
    /// doing both under one lock, so the second caller is refused even when
    /// it checked before the first had inserted anything.
    #[test]
    fn try_begin_refuses_a_second_session_while_one_is_still_active() {
        let tracker = BootstrapTracker::default();
        let first = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("the first session claims the slot");

        assert!(
            tracker
                .try_begin("rocky-9.8", "alpine-3.24", Uuid::new_v4())
                .is_none()
        );

        // The slot frees up again once the first session reaches a terminal
        // status — otherwise one bootstrap would block the feature forever.
        tracker.finish_ok(first);
        assert!(
            tracker
                .try_begin("rocky-9.8", "alpine-3.24", Uuid::new_v4())
                .is_some()
        );
    }

    #[test]
    fn set_status_from_only_applies_while_the_session_is_in_the_expected_status() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");

        assert!(tracker.set_status_from(id, BootstrapStatus::Booting, BootstrapStatus::Running));
        assert_eq!(tracker.get(id).unwrap().status, BootstrapStatus::Running);
        assert!(!tracker.set_status_from(id, BootstrapStatus::Booting, BootstrapStatus::Running));
    }

    #[test]
    fn finish_ok_records_succeeded_status_and_end_time() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");

        tracker.finish_ok(id);

        let snapshot = tracker.get(id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Succeeded);
        assert!(snapshot.ended_at_ms.is_some());
    }

    #[test]
    fn finish_err_records_failed_status_and_reason_in_the_log() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");

        tracker.finish_err(id, "download failed: connection reset");

        let snapshot = tracker.get(id).unwrap();
        assert_eq!(snapshot.status, BootstrapStatus::Failed);
        assert!(snapshot.log.contains("download failed"));
    }

    #[test]
    fn finish_err_from_only_fails_a_session_still_in_the_expected_status() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");

        tracker.set_status_from(id, BootstrapStatus::Booting, BootstrapStatus::Running);
        assert!(!tracker.finish_err_from(id, BootstrapStatus::Booting, "boot timed out"));
        assert_eq!(tracker.get(id).unwrap().status, BootstrapStatus::Running);

        assert!(tracker.finish_err_from(id, BootstrapStatus::Running, "script failed"));
        assert_eq!(tracker.get(id).unwrap().status, BootstrapStatus::Failed);
    }

    #[test]
    fn remove_evicts_a_tracked_session() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("ubuntu-26.04", "alpine-3.24", Uuid::new_v4())
            .expect("no other session is active");
        tracker.remove(id);
        assert!(tracker.get(id).is_none());
    }

    #[test]
    fn a_new_session_opens_on_the_builder_vm_step() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
            .expect("first session");
        let session = tracker.get(id).expect("session");

        assert_eq!(session.current_step, Some(BootstrapStep::StartingBuilderVm));
        assert_eq!(session.step_timeline.len(), 1);
        assert_eq!(
            session.step_timeline[0].outcome,
            BootstrapStepOutcome::Running
        );
        assert_eq!(session.step_timeline[0].ended_at_ms, None);
    }

    #[test]
    fn set_step_closes_the_previous_step_as_succeeded() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
            .expect("first session");

        tracker.set_step(id, BootstrapStep::InstallingSystem);
        let session = tracker.get(id).expect("session");

        assert_eq!(session.step_timeline.len(), 2);
        assert_eq!(
            session.step_timeline[0].outcome,
            BootstrapStepOutcome::Succeeded
        );
        assert!(session.step_timeline[0].ended_at_ms.is_some());
        assert_eq!(session.current_step, Some(BootstrapStep::InstallingSystem));
        assert_eq!(
            session.step_timeline[1].outcome,
            BootstrapStepOutcome::Running
        );
    }

    #[test]
    fn finishing_ok_closes_the_last_step_and_clears_the_current_one() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
            .expect("first session");
        tracker.set_step(id, BootstrapStep::Finalizing);

        tracker.finish_ok(id);
        let session = tracker.get(id).expect("session");

        assert_eq!(session.current_step, None);
        assert!(
            session
                .step_timeline
                .iter()
                .all(|run| run.outcome == BootstrapStepOutcome::Succeeded),
            "every step should be succeeded: {:?}",
            session.step_timeline
        );
    }

    #[test]
    fn failing_marks_the_step_that_was_open_and_carries_the_reason() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("rocky-9.8", "__microboot", Uuid::new_v4())
            .expect("first session");
        tracker.set_step(id, BootstrapStep::InstallingSystem);

        tracker.finish_err(id, "bootstrap script exited with code 1");
        let session = tracker.get(id).expect("session");

        assert_eq!(session.current_step, None);
        let failed = session
            .step_timeline
            .iter()
            .find(|run| run.outcome == BootstrapStepOutcome::Failed)
            .expect("a failed step");
        assert_eq!(failed.step, BootstrapStep::InstallingSystem);
        assert_eq!(
            failed.detail.as_deref(),
            Some("bootstrap script exited with code 1")
        );
        // The earlier step still counts as done, not failed.
        assert_eq!(
            session.step_timeline[0].outcome,
            BootstrapStepOutcome::Succeeded
        );
    }

    #[test]
    fn a_compare_and_set_failure_that_does_not_apply_leaves_the_timeline_alone() {
        let tracker = BootstrapTracker::default();
        let id = tracker
            .try_begin("alpine-3.24", "__microboot", Uuid::new_v4())
            .expect("first session");

        // Session is in `Booting`; this expects `Packaging`, so it must no-op.
        let applied = tracker.finish_err_from(id, BootstrapStatus::Packaging, "stale watcher");
        assert!(!applied);

        let session = tracker.get(id).expect("session");
        assert_eq!(session.current_step, Some(BootstrapStep::StartingBuilderVm));
        assert_eq!(
            session.step_timeline[0].outcome,
            BootstrapStepOutcome::Running
        );
    }
}
