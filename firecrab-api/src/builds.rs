//! In-process tracker for image-build sessions
//! (`POST /api/images/{alias}/build` and friends) — mirrors
//! `image_install::ImageInstallTracker`'s mechanics but keyed by a
//! generated build id, since a builder VM (and its console) is the
//! long-lived resource a session owns, not just an alias name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use firecrab_api_types::{BuildResponse, BuildStatus};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct BuildTracker {
    sessions: Arc<Mutex<HashMap<Uuid, BuildResponse>>>,
}

impl BuildTracker {
    /// Registers a new session in `Booting` and returns its id.
    pub fn begin(&self, source_alias: &str, vm_id: Uuid) -> Uuid {
        let build_id = Uuid::new_v4();
        let now = now_ms();
        let session = BuildResponse {
            build_id,
            source_alias: source_alias.to_owned(),
            target_alias: None,
            vm_id,
            status: BuildStatus::Booting,
            log: format!("[{}] builder VM starting", clock(now)),
            started_at_ms: now,
            ended_at_ms: None,
            had_package_action: false,
        };
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(build_id, session);
        build_id
    }

    pub fn get(&self, build_id: Uuid) -> Option<BuildResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&build_id)
            .cloned()
    }

    pub fn list(&self) -> Vec<BuildResponse> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn set_status(&self, build_id: Uuid, status: BuildStatus) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.status = status;
        }
    }

    /// Compare-and-set variant of [`set_status`](Self::set_status): only
    /// advances a session that is still in `expected`, returning whether it
    /// applied. Detached background tasks (the boot watcher and the package
    /// outcome recorder in `handlers::builds`) each own exactly one status
    /// transition, and must never clobber a status a later request already
    /// moved past — e.g. a slow boot watcher writing `Ready` over a session
    /// that has since reached `Finalizing`.
    pub fn set_status_from(
        &self,
        build_id: Uuid,
        expected: BuildStatus,
        next: BuildStatus,
    ) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&build_id) {
            Some(session) if session.status == expected => {
                session.status = next;
                true
            }
            _ => false,
        }
    }

    pub fn append_log(&self, build_id: Uuid, line: impl AsRef<str>) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.log.push('\n');
            session
                .log
                .push_str(&format!("[{}] {}", clock(now_ms()), line.as_ref()));
        }
    }

    pub fn finish_ok(&self, build_id: Uuid, target_alias: &str) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.status = BuildStatus::Succeeded;
            session.target_alias = Some(target_alias.to_owned());
            session.ended_at_ms = Some(now_ms());
        }
    }

    pub fn finish_err(&self, build_id: Uuid, reason: impl AsRef<str>) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(session) = sessions.get_mut(&build_id) {
            session.status = BuildStatus::Failed;
            session.log.push('\n');
            session
                .log
                .push_str(&format!("[{}] {}", clock(now_ms()), reason.as_ref()));
            session.ended_at_ms = Some(now_ms());
        }
    }

    /// Compare-and-set variant of [`finish_err`](Self::finish_err), for the
    /// same reason [`set_status_from`](Self::set_status_from) exists: a
    /// detached watcher must not turn a session that has already moved on
    /// (or already finished) into a fresh `Failed`.
    pub fn finish_err_from(
        &self,
        build_id: Uuid,
        expected: BuildStatus,
        reason: impl AsRef<str>,
    ) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&build_id) {
            Some(session) if session.status == expected => {
                session.status = BuildStatus::Failed;
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
    pub fn remove(&self, build_id: Uuid) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&build_id);
    }

    /// Marks that a package install/remove/update has completed on this
    /// session — `handlers::builds::finalize_build` (Task 9) refuses to
    /// register a template from a session where this is still `false`.
    pub fn mark_package_action_done(&self, build_id: Uuid) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&build_id)
        {
            session.had_package_action = true;
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn clock(epoch_ms: u64) -> String {
    // Plain epoch-seconds label (e.g. "1712s"), not a full timestamp —
    // good enough for a human skimming the log.
    format!("{}s", epoch_ms / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_then_snapshot_returns_a_booting_session() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Booting);
        assert_eq!(snapshot.source_alias, "alpine-3.24");
    }

    #[test]
    fn get_returns_none_for_an_unknown_build_id() {
        let tracker = BuildTracker::default();
        assert!(tracker.get(Uuid::new_v4()).is_none());
    }

    #[test]
    fn set_status_and_append_log_update_the_live_snapshot() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("ubuntu-26.04", Uuid::new_v4());

        tracker.append_log(build_id, "booted, waiting for network");
        tracker.set_status(build_id, BuildStatus::Ready);

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Ready);
        assert!(snapshot.log.contains("booted, waiting for network"));
    }

    #[test]
    fn finish_ok_records_target_alias_and_succeeded_status() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());

        tracker.finish_ok(build_id, "my-nginx-base");

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Succeeded);
        assert_eq!(snapshot.target_alias.as_deref(), Some("my-nginx-base"));
        assert!(snapshot.ended_at_ms.is_some());
    }

    #[test]
    fn finish_err_records_failed_status_and_reason_in_the_log() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("rocky-9", Uuid::new_v4());

        tracker.finish_err(build_id, "package install failed: exit 1");

        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Failed);
        assert!(snapshot.log.contains("package install failed"));
    }

    #[test]
    fn list_returns_every_tracked_session() {
        let tracker = BuildTracker::default();
        tracker.begin("alpine-3.24", Uuid::new_v4());
        tracker.begin("ubuntu-26.04", Uuid::new_v4());

        assert_eq!(tracker.list().len(), 2);
    }

    #[test]
    fn had_package_action_starts_false_and_flips_once_marked() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());
        assert!(!tracker.get(build_id).unwrap().had_package_action);

        tracker.mark_package_action_done(build_id);

        assert!(tracker.get(build_id).unwrap().had_package_action);
    }

    /// The boot watcher owns exactly one `Booting -> Ready` transition, so
    /// it must be a no-op once anything else has advanced the session.
    #[test]
    fn set_status_from_only_applies_while_the_session_is_in_the_expected_status() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());

        assert!(tracker.set_status_from(build_id, BuildStatus::Booting, BuildStatus::Ready));
        assert_eq!(tracker.get(build_id).unwrap().status, BuildStatus::Ready);

        // Already past `Booting` — a late watcher must not rewind it.
        tracker.set_status(build_id, BuildStatus::Finalizing);
        assert!(!tracker.set_status_from(build_id, BuildStatus::Booting, BuildStatus::Ready));
        assert_eq!(
            tracker.get(build_id).unwrap().status,
            BuildStatus::Finalizing
        );
        assert!(!tracker.set_status_from(Uuid::new_v4(), BuildStatus::Booting, BuildStatus::Ready));
    }

    #[test]
    fn finish_err_from_only_fails_a_session_still_in_the_expected_status() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());

        tracker.set_status(build_id, BuildStatus::Ready);
        assert!(!tracker.finish_err_from(build_id, BuildStatus::Booting, "boot timed out"));
        assert_eq!(tracker.get(build_id).unwrap().status, BuildStatus::Ready);

        assert!(tracker.finish_err_from(build_id, BuildStatus::Ready, "boot timed out"));
        let snapshot = tracker.get(build_id).unwrap();
        assert_eq!(snapshot.status, BuildStatus::Failed);
        assert!(snapshot.log.contains("boot timed out"));
        assert!(snapshot.ended_at_ms.is_some());
    }

    #[test]
    fn remove_evicts_a_tracked_session() {
        let tracker = BuildTracker::default();
        let build_id = tracker.begin("alpine-3.24", Uuid::new_v4());
        assert!(tracker.get(build_id).is_some());

        tracker.remove(build_id);

        assert!(tracker.get(build_id).is_none());
    }
}
