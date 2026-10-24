use firecrab_stack::{ApplyState, StepState};

#[test]
fn durable_lifecycle_names_match_json_and_distinguish_polling_from_terminal_results() {
    for (state, name, terminal) in [
        (ApplyState::Queued, "queued", false),
        (ApplyState::Running, "running", false),
        (ApplyState::Succeeded, "succeeded", true),
        (ApplyState::Failed, "failed", true),
        (ApplyState::Interrupted, "interrupted", true),
    ] {
        assert_eq!(state.as_str(), name);
        assert_eq!(serde_json::to_value(state).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<ApplyState>(name.into()).unwrap(),
            state
        );
        assert_eq!(state.is_terminal(), terminal);
    }
    for (state, name) in [
        (StepState::Pending, "pending"),
        (StepState::Running, "running"),
        (StepState::Succeeded, "succeeded"),
        (StepState::Failed, "failed"),
        (StepState::Skipped, "skipped"),
    ] {
        assert_eq!(state.as_str(), name);
        assert_eq!(serde_json::to_value(state).unwrap(), name);
        assert_eq!(
            serde_json::from_value::<StepState>(name.into()).unwrap(),
            state
        );
    }
    assert!(serde_json::from_value::<ApplyState>("unknown".into()).is_err());
}
