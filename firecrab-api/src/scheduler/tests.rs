use super::*;
use crate::persistence::{ScheduleMutation, Store};
use firecrab_api_types::{ScheduleRunStatus, ScheduleSpec};
use uuid::Uuid;

fn instant(text: &str) -> i64 {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .timestamp_millis()
}

fn record(timing: ScheduleTiming, now: i64) -> ScheduleResponse {
    ScheduleResponse {
        id: Uuid::new_v4(),
        spec: ScheduleSpec {
            name: "workday".to_owned(),
            vm_id: Uuid::new_v4(),
            action: ScheduleAction::Start,
            timing,
            enabled: true,
        },
        next_run_at_ms: None,
        created_at_ms: now,
        updated_at_ms: now,
        last_run: None,
    }
}

fn recurring() -> ScheduleTiming {
    ScheduleTiming::Cron {
        expression: "0 9 * * Mon-Fri".to_owned(),
        time_zone: "Asia/Seoul".to_owned(),
    }
}

#[test]
fn time_zones_once_and_weekdays() {
    let now = instant("2026-10-23T08:59:59+09:00");
    assert_eq!(
        next_run(&recurring(), now).unwrap(),
        Some(instant("2026-10-23T09:00:00+09:00"))
    );
    assert_eq!(
        next_run(&recurring(), now + 1000).unwrap(),
        Some(instant("2026-10-26T09:00:00+09:00"))
    );
    let once = ScheduleTiming::Once {
        at: "2026-10-23T09:00:00+09:00".to_owned(),
    };
    assert_eq!(next_run(&once, now).unwrap(), Some(now + 1000));
    assert_eq!(next_run(&once, now + 1000).unwrap(), None);
    assert!(
        next_run(
            &ScheduleTiming::Once {
                at: "2026-10-23T09:00".to_owned()
            },
            now
        )
        .is_err()
    );
    for (expression, zone) in [
        ("* * * * * *", "UTC"),
        ("invalid", "UTC"),
        ("0 9 * * *", "Mars/Base"),
    ] {
        assert!(
            next_run(
                &ScheduleTiming::Cron {
                    expression: expression.to_owned(),
                    time_zone: zone.to_owned()
                },
                now
            )
            .is_err()
        );
    }
}

#[test]
fn daylight_saving_skips_nonexistent_local_time() {
    let timing = ScheduleTiming::Cron {
        expression: "30 2 * * *".to_owned(),
        time_zone: "America/New_York".to_owned(),
    };
    assert_eq!(
        next_run(&timing, instant("2026-03-08T00:00:00-05:00")).unwrap(),
        Some(instant("2026-03-09T02:30:00-04:00"))
    );
    let timing = ScheduleTiming::Cron {
        expression: "30 1 * * *".to_owned(),
        time_zone: "America/New_York".to_owned(),
    };
    let next = next_run(&timing, instant("2026-11-01T00:00:00-04:00"))
        .unwrap()
        .unwrap();
    assert!(next > instant("2026-11-01T00:00:00-04:00"));
    assert_eq!(next, instant("2026-11-01T01:30:00-04:00"));
    assert_eq!(
        next_run(&timing, next).unwrap(),
        Some(instant("2026-11-02T01:30:00-05:00"))
    );
    assert_eq!(
        next_run(&timing, instant("2026-11-01T01:10:00-05:00")).unwrap(),
        Some(instant("2026-11-02T01:30:00-05:00"))
    );
}

#[test]
fn claims_coalesce_missed_runs_and_prevent_overlap() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("test.db")).unwrap();
    let now = instant("2026-10-23T08:00:00+09:00");
    let mut schedule = record(recurring(), now);
    schedule.next_run_at_ms = Some(instant("2026-10-20T09:00:00+09:00"));
    store.insert_schedule(&schedule).unwrap();
    let late = instant("2026-10-23T15:00:00+09:00");
    let claimed = store.claim_schedules(late, 4).unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(store.claim_schedules(late + 60_000, 4).unwrap().is_empty());
    assert_eq!(
        store
            .mutate_schedule(schedule.id, Some(&schedule.spec), None, late)
            .unwrap(),
        ScheduleMutation::Busy
    );
    assert_eq!(
        store
            .mutate_schedule(schedule.id, None, None, late)
            .unwrap(),
        ScheduleMutation::Busy
    );
    store
        .finish_schedule_run(&claimed[0].1, late, None, false, late)
        .unwrap();
    let saved = store.schedule(schedule.id).unwrap().unwrap();
    assert_eq!(
        saved.next_run_at_ms,
        Some(instant("2026-10-26T09:00:00+09:00"))
    );
    assert_eq!(saved.last_run.unwrap().status, ScheduleRunStatus::Succeeded);
}

#[test]
fn once_is_consumed_before_execution_and_restart_does_not_replay_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("test.db");
    let now = instant("2026-10-23T09:00:00+09:00");
    let store = Store::open(&path).unwrap();
    let mut schedule = record(
        ScheduleTiming::Once {
            at: "2026-10-23T09:00:00+09:00".to_owned(),
        },
        now,
    );
    schedule.next_run_at_ms = Some(now);
    store.insert_schedule(&schedule).unwrap();
    assert_eq!(store.claim_schedules(now, 1).unwrap().len(), 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    store.recover_schedules(now + 1000).unwrap();
    let saved = store.schedule(schedule.id).unwrap().unwrap();
    assert!(!saved.spec.enabled);
    assert_eq!(
        saved.last_run.unwrap().status,
        ScheduleRunStatus::Interrupted
    );
    assert!(store.claim_schedules(now + 1000, 4).unwrap().is_empty());
}

#[test]
fn disable_during_run_is_preserved_and_reenable_skips_past_occurrences() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("test.db")).unwrap();
    let now = instant("2026-10-23T09:00:00+09:00");
    let mut schedule = record(recurring(), now);
    schedule.next_run_at_ms = Some(now);
    store.insert_schedule(&schedule).unwrap();
    let run = store.claim_schedules(now, 1).unwrap().remove(0).1;
    assert_eq!(
        store
            .mutate_schedule(schedule.id, None, Some(false), now)
            .unwrap(),
        ScheduleMutation::Done
    );
    assert_eq!(
        store
            .mutate_schedule(schedule.id, None, Some(true), now)
            .unwrap(),
        ScheduleMutation::Busy
    );
    store
        .finish_schedule_run(
            &run,
            now + 1,
            Some("operation failed".to_owned()),
            false,
            now + 1,
        )
        .unwrap();
    let saved = store.schedule(schedule.id).unwrap().unwrap();
    assert!(!saved.spec.enabled);
    assert_eq!(saved.next_run_at_ms, None);
    assert_eq!(saved.last_run.unwrap().status, ScheduleRunStatus::Failed);
    store
        .mutate_schedule(schedule.id, None, Some(true), now + 1)
        .unwrap();
    assert_eq!(
        store.schedule(schedule.id).unwrap().unwrap().next_run_at_ms,
        Some(instant("2026-10-26T09:00:00+09:00"))
    );
    store
        .mutate_schedule(schedule.id, None, None, now + 2)
        .unwrap();
    assert!(store.schedule_runs(schedule.id).unwrap().is_empty());
}

#[test]
fn independent_connections_cannot_claim_the_same_occurrence() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("test.db");
    let store = Store::open(&path).unwrap();
    let now = instant("2026-10-23T09:00:00+09:00");
    let mut schedule = record(recurring(), now);
    schedule.next_run_at_ms = Some(now);
    store.insert_schedule(&schedule).unwrap();
    let second = Store::open(&path).unwrap();
    let other = std::thread::spawn(move || second.claim_schedules(now, 4).unwrap());
    let count = store.claim_schedules(now, 4).unwrap().len() + other.join().unwrap().len();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn deleted_target_fails_and_disables_schedule_without_host_operations() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, root.path().join("test.db"))
        .await
        .unwrap();
    state.network =
        crate::network::NetworkClient::with_socket_path(root.path().join("unused-helper.sock"));
    let now = Utc::now().timestamp_millis();
    let mut schedule = record(recurring(), now);
    schedule.next_run_at_ms = Some(now);
    state.store.insert_schedule(&schedule).unwrap();
    let (schedule, run) = state.store.claim_schedules(now, 1).unwrap().remove(0);
    execute(state.clone(), schedule.clone(), run).await;
    let saved = state.store.schedule(schedule.id).unwrap().unwrap();
    assert!(!saved.spec.enabled);
    assert!(saved.next_run_at_ms.is_none());
    assert_eq!(
        saved.last_run.unwrap().error.unwrap(),
        "target VM no longer exists; schedule disabled"
    );
}

#[test]
fn scheduler_ownership_is_exclusive_until_its_process_releases_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("test.db");
    let first = Store::open(&path).unwrap();
    let second = Store::open(&path).unwrap();
    let lock = first.scheduler_lock().unwrap();
    assert!(second.scheduler_lock().is_err());
    drop(lock);
    assert!(second.scheduler_lock().is_ok());
}

#[test]
fn history_is_bounded_even_when_each_run_is_interrupted() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("test.db")).unwrap();
    let now = instant("2026-10-23T09:00:00Z");
    let mut schedule = record(
        ScheduleTiming::Cron {
            expression: "* * * * *".to_owned(),
            time_zone: "UTC".to_owned(),
        },
        now,
    );
    schedule.next_run_at_ms = Some(now);
    store.insert_schedule(&schedule).unwrap();
    for i in 0..105 {
        let tick = now + i * 60_000;
        assert_eq!(store.claim_schedules(tick, 1).unwrap().len(), 1);
        store.recover_schedules(tick + 1).unwrap();
    }
    let history = store.schedule_runs(schedule.id).unwrap();
    assert_eq!(history.len(), 100);
    assert_eq!(history[0].scheduled_at_ms, now + 104 * 60_000);
    let conn = rusqlite::Connection::open(root.path().join("test.db")).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schedule_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 100);
}

#[test]
fn backwards_clock_and_idempotent_enable_do_not_repeat_consumed_instants() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("test.db")).unwrap();
    let now = instant("2026-10-23T09:00:00+09:00");
    let mut schedule = record(recurring(), now);
    schedule.next_run_at_ms = Some(now);
    store.insert_schedule(&schedule).unwrap();
    store
        .mutate_schedule(schedule.id, None, Some(true), now - 1)
        .unwrap();
    assert_eq!(
        store.schedule(schedule.id).unwrap().unwrap().next_run_at_ms,
        Some(now)
    );
    let run = store.claim_schedules(now, 1).unwrap().remove(0).1;
    store
        .finish_schedule_run(&run, now - 60_000, None, false, now - 60_000)
        .unwrap();
    assert!(
        store
            .schedule(schedule.id)
            .unwrap()
            .unwrap()
            .next_run_at_ms
            .unwrap()
            > now
    );
    assert!(store.claim_schedules(now, 1).unwrap().is_empty());
}

#[tokio::test]
async fn persisted_schedules_cannot_mutate_pool_members() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, root.path().join("test.db"))
        .await
        .unwrap();
    state.network =
        crate::network::NetworkClient::with_socket_path(root.path().join("unused-helper.sock"));
    let now = Utc::now().timestamp_millis();
    let vm_id = Uuid::new_v4();
    let mut vm = crate::handlers::vms::test_support::record("pool-scheduled", vm_id);
    vm.purpose = crate::model::VmPurpose::Pool;
    vm.state = crate::model::VmState::Running;
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm_id, vm);
    for action in [ScheduleAction::Start, ScheduleAction::Stop] {
        let mut schedule = record(recurring(), now);
        schedule.spec.vm_id = vm_id;
        schedule.spec.action = action;
        schedule.next_run_at_ms = Some(now);
        state.store.insert_schedule(&schedule).unwrap();
        let (claimed, run) = state.store.claim_schedules(now, 1).unwrap().remove(0);
        execute(state.clone(), claimed, run).await;
        let saved = state.store.schedule(schedule.id).unwrap().unwrap();
        assert!(!saved.spec.enabled);
        let last = saved.last_run.unwrap();
        assert_eq!(last.status, ScheduleRunStatus::Failed);
        assert!(last.error.unwrap().contains("pool members"));
        assert_eq!(
            state.vms.lock().unwrap()[&vm_id].state,
            crate::model::VmState::Running
        );
    }
}

#[tokio::test]
async fn worker_calls_existing_stop_handler_and_records_success_or_invalid_state() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let mut state = AppState::with_db_file(templates, root.path().join("test.db"))
        .await
        .unwrap();
    let socket = root.path().join("fake-helper.sock");
    let helper = crate::network::test_support::spawn_always_ok_helper(&socket);
    state.network = crate::network::NetworkClient::with_socket_path(socket);
    let now = Utc::now().timestamp_millis();
    let mut schedule = record(recurring(), now);
    schedule.spec.action = ScheduleAction::Stop;
    let vm: crate::model::VmRecord = serde_json::from_value(serde_json::json!({
        "id":schedule.spec.vm_id,"name":"scheduled-test","state":"running","template":"unused","cpu":1,"ram":128,"micro_network_id":Uuid::new_v4()
    })).unwrap();
    state.store.insert(&vm).unwrap();
    state.vms.lock().unwrap().insert(vm.id, vm);
    schedule.next_run_at_ms = Some(now);
    state.store.insert_schedule(&schedule).unwrap();
    let (claimed, run) = state.store.claim_schedules(now, 1).unwrap().remove(0);
    execute(state.clone(), claimed, run).await;
    assert_eq!(
        state.vms.lock().unwrap()[&schedule.spec.vm_id].state,
        crate::model::VmState::Stopped
    );
    assert_eq!(
        state
            .store
            .schedule(schedule.id)
            .unwrap()
            .unwrap()
            .last_run
            .unwrap()
            .status,
        ScheduleRunStatus::Succeeded
    );
    // Stopping an already stopped VM follows the same conflict rules as REST.
    schedule.id = Uuid::new_v4();
    state.store.insert_schedule(&schedule).unwrap();
    let (claimed, run) = state.store.claim_schedules(now, 1).unwrap().remove(0);
    execute(state.clone(), claimed, run).await;
    let saved = state.store.schedule(schedule.id).unwrap().unwrap();
    assert_eq!(
        saved.last_run.as_ref().unwrap().status,
        ScheduleRunStatus::Failed
    );
    assert!(
        saved
            .last_run
            .unwrap()
            .error
            .unwrap()
            .contains("invalid_state")
    );
    helper.abort();
}

#[tokio::test]
async fn scheduler_tick_restores_due_work_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let templates =
        crate::templates::TemplateRegistry::from_specs(root.path(), std::iter::empty()).unwrap();
    let state = AppState::with_db_file(templates, root.path().join("test.db"))
        .await
        .unwrap();
    let now = Utc::now().timestamp_millis();
    let mut schedule = record(recurring(), now);
    schedule.next_run_at_ms = Some(now - 60_000);
    state.store.insert_schedule(&schedule).unwrap();
    spawn(state.clone(), state.store.scheduler_lock().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let saved = state.store.schedule(schedule.id).unwrap().unwrap();
            if saved
                .last_run
                .is_some_and(|run| run.status == ScheduleRunStatus::Failed)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.store.schedule_runs(schedule.id).unwrap().len(), 1);
}
