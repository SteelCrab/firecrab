use super::*;
fn task() -> Value {
    json!({"id":"11111111-1111-4111-8111-111111111111","projectId":"22222222-2222-4222-8222-222222222222","attemptId":"33333333-3333-4333-8333-333333333333","request":{"kind":"code","input":"Fix the test","selection":null,"retryOf":null},"selection":{"connectionId":"44444444-4444-4444-8444-444444444444","model":"model"},"state":"result","createdAt":1,"updatedAt":2,"result":"Done","errorCode":null,"error":null,"events":[{"sequence":0,"at":1,"name":"run_command","input":"{}","output":"bounded output","stderr":"","exitCode":0}],"changes":[{"path":"main.txt","before":"old","after":"new","beforeMode":420,"afterMode":420,"applied":false}]})
}
#[test]
fn full_task_projection_matches_the_authoritative_rest_summary_contract() {
    let value = task();
    let typed: firecrab_api_types::SandboxTask = serde_json::from_value(value.clone()).unwrap();
    let wire = serde_json::to_value(firecrab_api_types::SandboxTaskSummary::from(&typed)).unwrap();
    assert_eq!(summary(value), wire);
}
#[test]
fn late_poll_does_not_erase_a_newer_mutation_or_older_loaded_history() {
    let first = json!({"id":"first","createdAt":1,"updatedAt":9,"state":"merged"});
    let second = json!({"id":"second","createdAt":2,"updatedAt":3,"state":"in_progress"});
    let mut rows = vec![second.clone(), first.clone()];
    merge_tasks(
        &mut rows,
        [json!({"id":"first","createdAt":1,"updatedAt":2,"state":"result"})],
    );
    assert_eq!(rows, vec![first, second]);
}
fn ready() -> App {
    let mut a = App::default();
    a.route = "ai-sandbox".into();
    a.locale = "en".into();
    a.loaded = true;
    a.sandbox.project = "project".into();
    a.sandbox.projects = Some(vec![
        json!({"id":"project","spec":{"name":"Project","selection":{"connectionId":"connection","model":"model"},"sourcePath":"/work/project","vmId":"vm","instructions":""}}),
    ]);
    a.sandbox.catalogs.insert(
        "connection".into(),
        json!({"models":[{"id":"model","verified":true,"toolsVerified":false}]}),
    );
    a.sandbox.tasks_loaded = true;
    a.sandbox.input = "Question".into();
    a.vms.push(crate::model::Vm {
        id: "vm".into(),
        state: "running".into(),
        ..Default::default()
    });
    a
}
#[test]
fn verified_default_and_override_require_current_code_capability_and_running_target() {
    let mut a = ready();
    assert!(can_send(&a));
    a.sandbox.selection = json!({"connectionId":"missing","model":"model"});
    assert!(!can_send(&a));
    a.sandbox.selection = Value::Null;
    a.sandbox.kind = "code".into();
    assert!(!can_send(&a));
    a.sandbox.catalogs.get_mut("connection").unwrap()["models"][0]["toolsVerified"] = json!(true);
    assert!(can_send(&a));
    a.vms[0].state = "stopped".into();
    assert!(!can_send(&a));
}
#[test]
fn provider_text_paths_and_diff_contents_are_escaped_in_all_review_surfaces() {
    let mut a = ready();
    let attack = "<img id=attack onerror='bad'>&\"";
    let mut value = task();
    value["request"]["input"] = json!(attack);
    value["result"] = json!(attack);
    value["changes"][0]["path"] = json!(attack);
    value["changes"][0]["after"] = json!(attack);
    a.sandbox.tasks.push(summary(value.clone()));
    a.sandbox.review = Some(Review {
        id: "task".into(),
        task: Some(value),
        tab: "changes".into(),
        paths: BTreeSet::new(),
        error: String::new(),
        load_error: String::new(),
        busy: false,
        polling: false,
    });
    let html = content(&a);
    assert!(!html.contains("<img id=attack"));
    assert!(html.contains(&escape(attack)));
}
