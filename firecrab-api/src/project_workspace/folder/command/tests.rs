use super::*;
fn command() -> SandboxGuestCommand {
    serde_json::from_value(serde_json::json!({"version":1,"argv":["printf","literal $(id)"],"cwd":"api","env":{"EXPLICIT":"value"},"stdin":"input","limits":{"wallSeconds":3,"cpuSeconds":2,"memoryMib":64,"outputBytes":8192}})).unwrap()
}
#[test]
fn frame_keeps_literal_arguments_limits_and_identity_without_root_shell_interpolation() {
    let id = Uuid::new_v4();
    let bytes = frame(id, 1, Some(&command())).unwrap();
    assert_eq!(&bytes[..4], b"FCX1");
    assert_eq!(&bytes[8..24], id.as_bytes());
    assert_eq!(number(&bytes, 24) as usize, bytes.len() - 28);
    assert!(
        bytes
            .windows(b"literal $(id)".len())
            .any(|w| w == b"literal $(id)")
    );
    assert_eq!(number(&bytes, 28), 3);
    assert_eq!(number(&bytes, 40), 8192);
    assert!(frame(Uuid::nil(), 1, Some(&command())).is_err());
    assert!(frame(id, 4, Some(&command())).is_err());
    let mut bad = command();
    bad.cwd = "../other".into();
    assert!(frame(id, 1, Some(&bad)).is_err());
    bad = command();
    bad.limits.wall_seconds = 121;
    assert!(frame(id, 1, Some(&bad)).is_err());
    assert_eq!(frame(id, 4, None).unwrap().len(), 28);
}
fn response(state: u32, exit: u32, flags: u32) -> Vec<u8> {
    let mut b = vec![0; 80];
    b[..4].copy_from_slice(b"FCXR");
    b[4..8].copy_from_slice(&72u32.to_be_bytes());
    b[8..12].copy_from_slice(b"FCXS");
    for (at, n) in [
        (12, state),
        (16, 100001),
        (24, 42),
        (28, 43),
        (64, exit),
        (68, flags),
    ] {
        b[at..at + 4].copy_from_slice(&n.to_be_bytes());
    }
    b[32..40].copy_from_slice(&1u64.to_be_bytes());
    b[40..48].copy_from_slice(&2u64.to_be_bytes());
    b[48..64].copy_from_slice(Uuid::new_v4().as_bytes());
    b
}
#[test]
fn replies_require_exact_bounds_boot_process_and_cleanup_proofs() {
    let done = decode(&response(2, 7, 0)).unwrap();
    assert_eq!(done.exit, Some(7));
    assert!(!done.cleanup_required);
    assert!(decode(&response(2, u32::MAX, 0)).is_err());
    assert!(decode(&response(2, 0, 1)).is_err());
    assert!(decode(&response(1, 0, 0)).is_err());
    assert!(decode(&response(4, 0, 0)).is_err());
    assert!(decode(&response(3, u32::MAX, 32)).is_err());
    let partial = decode(&response(3, u32::MAX, 1)).unwrap();
    assert!(partial.cleanup_required);
    assert_eq!(partial.exit, None);
    let mut bad = response(2, 0, 0);
    bad.push(1);
    assert!(decode(&bad).is_err());
    bad = response(2, 0, 0);
    bad[48..64].fill(0);
    assert!(decode(&bad).is_err());
}

#[test]
fn restarted_runtime_can_only_observe_or_cancel_the_same_verified_receipt() {
    let target = Target {
        project: Uuid::new_v4(),
        database: Uuid::new_v4(),
        volume: Uuid::new_v4(),
        generation: 1,
        runtime: guest::Binding {
            vm_id: Uuid::new_v4(),
            runtime_id: Uuid::new_v4(),
        },
        caller: Caller::default(),
        directory: "api".into(),
    };
    let mut binding = target.expected();
    assert!(pin_receipt(&target, &binding, target.runtime, 1).is_ok());
    let runtime = guest::Binding {
        runtime_id: Uuid::new_v4(),
        ..target.runtime
    };
    binding.verified_runtime_id = Some(runtime.runtime_id);
    assert!(pin_receipt(&target, &binding, runtime, 1).is_err());
    for action in [2, 3] {
        assert!(pin_receipt(&target, &binding, runtime, action).is_ok());
        for changed in 0..6 {
            let mut other = binding.clone();
            match changed {
                0 => other.project_id = Uuid::new_v4(),
                1 => other.database_id = Uuid::new_v4(),
                2 => other.volume_id = Uuid::new_v4(),
                3 => other.generation += 1,
                4 => other.vm_id = Uuid::new_v4(),
                _ => other.provisioning = true,
            }
            assert!(pin_receipt(&target, &other, runtime, action).is_err());
        }
        let mut unverified = binding.clone();
        unverified.verified_runtime_id = Some(Uuid::new_v4());
        assert!(pin_receipt(&target, &unverified, runtime, action).is_err());
        unverified.verified_runtime_id = None;
        assert!(pin_receipt(&target, &unverified, runtime, action).is_err());
        let foreign = guest::Binding {
            vm_id: Uuid::new_v4(),
            ..runtime
        };
        assert!(pin_receipt(&target, &binding, foreign, action).is_err());
    }
    assert!(pin_receipt(&target, &binding, runtime, 4).is_err());
}
