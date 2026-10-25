use std::process::{Command, Stdio};

#[test]
fn legacy_boolean_flags_cannot_enable_a_plaintext_remote_api() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_firecrab-api"))
        .current_dir(directory.path())
        .env("FIRECRAB_BIND_ADDR", "0.0.0.0:5523")
        .env("FIRECRAB_AUTHENTICATION_ENABLED", "true")
        .env("FIRECRAB_TLS_ENABLED", "true")
        .env_remove("FIRECRAB_TLS_CERT")
        .env_remove("FIRECRAB_TLS_KEY")
        .env_remove("FIRECRAB_TLS_CA")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("insecure API did not refuse startup");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("non-loopback bind requires FIRECRAB_TLS_CERT")
    );
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        0,
        "TLS validation must precede persisted state initialization"
    );
}
