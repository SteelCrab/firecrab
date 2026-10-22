use std::process::Command;

use serde_json::Value;

const EXAMPLE: &str = include_str!("../../examples/stacks/basic.yaml");

fn validate(source: &[u8]) -> std::process::Output {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stack.yaml");
    std::fs::write(&path, source).unwrap();
    Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .args([
            "--api",
            "invalid://no-api",
            "--host",
            "missing",
            "stack",
            "validate",
            "--json",
            "--file",
        ])
        .arg(path)
        .env(
            "FIRECRAB_CONFIG_DIR",
            directory.path().join("missing-config"),
        )
        .output()
        .unwrap()
}

#[test]
fn validates_offline_even_with_unusable_host_settings() {
    let output = validate(EXAMPLE.as_bytes());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["valid"], true);
    assert_eq!(report["stack"], "demo");
    assert_eq!(report["resources"]["microVMs"], 1);
    assert_eq!(report["errors"], serde_json::json!([]));
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("APP_MODE")
    );
}

#[test]
fn invalid_configuration_exits_one_with_a_json_report() {
    let output = validate(
        EXAMPLE
            .replace("network: app", "network: missing")
            .as_bytes(),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["valid"], false);
    assert!(!report["errors"].as_array().unwrap().is_empty());
}

#[test]
fn parse_failures_do_not_echo_values_or_yaml_excerpts() {
    let output = validate(
        EXAMPLE
            .replace("cpu: 1", "cpu: private-token-DO-NOT-PRINT")
            .as_bytes(),
    );
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["valid"], false);
    assert!(
        !format!("{report}{}", String::from_utf8_lossy(&output.stderr)).contains("DO-NOT-PRINT")
    );
}

#[test]
fn rejects_oversized_and_non_utf8_input() {
    for source in [
        vec![b' '; firecrab_stack::MAX_MANIFEST_BYTES + 1],
        vec![0xff, 0xfe],
    ] {
        let output = validate(&source);
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["valid"], false);
    }
}

#[test]
fn missing_file_is_reported_as_json() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_firecrab"))
        .args(["stack", "validate", "--json", "--file"])
        .arg(directory.path().join("missing.yaml"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["errors"][0]["message"],
        "could not open manifest file"
    );
}

#[test]
fn human_output_reports_counts_and_failure_coordinates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stack.yaml");
    std::fs::write(&path, EXAMPLE).unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_firecrab"))
            .args(["stack", "validate", "--file"])
            .arg(&path)
            .output()
            .unwrap()
    };
    let output = invoke();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 networks, 1 VMs, 1 services"));
    std::fs::write(
        &path,
        EXAMPLE.replace("cpu: 1", "cpu: private-token-DO-NOT-PRINT"),
    )
    .unwrap();
    let output = invoke();
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("line "));
    assert!(!error.contains("DO-NOT-PRINT"));
}
