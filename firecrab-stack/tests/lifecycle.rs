use firecrab_stack::{
    Action, Binding, Inventory, LifecyclePolicy, ResourceKind, StackRecord, parse, plan,
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn destructive_flags_are_explicit_and_empty_stacks_require_pruning() {
    let empty = "apiVersion: firecrab.dev/v1alpha1\nkind: Stack\nname: demo\n";
    assert!(parse(empty).is_err());
    assert!(parse(&format!("{empty}lifecycle:\n  prune: true\n")).is_ok());
    assert!(parse(&format!("{empty}lifecycle:\n  replace: true\n")).is_err());
    let example = include_str!("../../examples/stacks/basic.yaml");
    let stack = parse(example).unwrap();
    assert!(stack.lifecycle.is_default());
    assert!(
        serde_json::to_value(&stack)
            .unwrap()
            .get("lifecycle")
            .is_none()
    );
    assert!(parse(&format!("{example}\nlifecycle:\n  deleteVmDisks: true\n")).is_err());
    assert!(
        serde_json::from_value::<LifecyclePolicy>(json!({"prune":true,"destroyEverything":true}))
            .is_err()
    );
}

#[test]
fn pruning_deletes_only_explicit_owned_uuids_and_policy_changes_invalidate_approval() {
    let mut stack = parse("apiVersion: firecrab.dev/v1alpha1\nkind: Stack\nname: demo\nmicroNetworks:\n  keep:\n    name: keep\n    subnetCidr: 172.31.20.0/24\n").unwrap();
    let record = StackRecord {
        id: Uuid::new_v4(),
        name: "demo".into(),
        revision: 1,
        bindings: vec![Binding {
            kind: ResourceKind::Network,
            key: "old".into(),
            resource_id: Uuid::new_v4(),
        }],
    };
    let inventory = Inventory::default();
    let retained = plan(&stack, &record, &inventory).unwrap();
    assert_eq!(retained.changes.last().unwrap().action, Action::Retain);
    stack.lifecycle.prune = true;
    let pruned = plan(&stack, &record, &inventory).unwrap();
    assert_ne!(retained.fingerprint, pruned.fingerprint);
    assert_eq!(pruned.changes.last().unwrap().action, Action::Delete);
    assert_eq!(
        pruned.changes.last().unwrap().resource_id,
        Some(record.bindings[0].resource_id)
    );
    assert_eq!(pruned.lifecycle, stack.lifecycle);
    assert_eq!(serde_json::to_value(Action::Delete).unwrap(), "delete");
    assert_eq!(Action::Delete.as_str(), "delete");
}
