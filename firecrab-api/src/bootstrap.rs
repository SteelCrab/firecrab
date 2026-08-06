//! In-process tracker for from-scratch distro bootstrap sessions
//! (`POST /api/images/{alias}/bootstrap` and friends) — mirrors
//! `builds::BuildTracker`'s mechanics exactly, but for a session kind whose
//! terminal action is "package as a `.tar.zst`" rather than "register a
//! template directly", so it's kept as its own tracker/type rather than
//! overloading `BuildTracker`'s `BuildStatus` with states that don't apply
//! to a customize-an-installed-template session (see
//! `docs/superpowers/specs/2026-08-03-m2image-web-rebuild-design.md`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{BootstrapResponse, BootstrapStatus};
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
    /// returning whether it applied — same reasoning as
    /// `BuildTracker::set_status_from` (a detached watcher must never
    /// clobber a status a later request already moved past).
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

    pub fn append_log(&self, id: Uuid, line: impl AsRef<str>) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&id)
        {
            session.log.push('\n');
            session
                .log
                .push_str(&format!("[{}] {}", clock(now_ms()), line.as_ref()));
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
            session
                .log
                .push_str(&format!("[{}] {}", clock(now_ms()), reason.as_ref()));
            session.ended_at_ms = Some(now_ms());
        }
    }

    /// Compare-and-set variant of [`finish_err`](Self::finish_err) — for
    /// the same reason `BuildTracker::finish_err_from` exists.
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
                session
                    .log
                    .push_str(&format!("[{}] {}", clock(now_ms()), reason.as_ref()));
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
            log: format!("[{}] builder VM starting", clock(now)),
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn clock(epoch_ms: u64) -> String {
    format!("{}s", epoch_ms / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                .try_begin("rocky-9", "alpine-3.24", Uuid::new_v4())
                .is_none()
        );

        // The slot frees up again once the first session reaches a terminal
        // status — otherwise one bootstrap would block the feature forever.
        tracker.finish_ok(first);
        assert!(
            tracker
                .try_begin("rocky-9", "alpine-3.24", Uuid::new_v4())
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
}
