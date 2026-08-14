use super::*;

use std::os::unix::fs::PermissionsExt;

fn tree_with_services() -> (tempfile::TempDir, ProvisionedRootfs) {
    let directory = tempfile::tempdir().expect("create fixture directory");
    let tree = directory.path().join("tree");
    std::fs::create_dir_all(tree.join("etc/firecrab/services.d")).expect("services.d");
    std::fs::create_dir_all(tree.join("sbin")).expect("sbin");
    std::fs::write(tree.join("sbin/init"), b"injected-init").expect("init");
    (
        directory,
        ProvisionedRootfs {
            path: tree,
            toolbox: Sha256Digest::of_bytes(b"toolbox-fixture"),
        },
    )
}

fn config_json(config: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": config,
        "rootfs": { "type": "layers", "diff_ids": [] }
    }))
    .expect("serialize config")
}

#[test]
fn process_config_joins_entrypoint_and_cmd() {
    let process = OciProcessConfig::from_image_config(&config_json(serde_json::json!({
        "Entrypoint": ["nginx"],
        "Cmd": ["-g", "daemon off;"],
        "Env": ["PATH=/usr/bin", "NGINX=1"],
        "WorkingDir": "/usr/share/nginx/html"
    })))
    .expect("parse process config");

    assert_eq!(process.entrypoint(), &["nginx"]);
    assert_eq!(process.cmd(), &["-g", "daemon off;"]);
    assert_eq!(process.env(), &["PATH=/usr/bin", "NGINX=1"]);
    assert_eq!(process.working_dir(), "/usr/share/nginx/html");
    assert_eq!(process.argv(), ["nginx", "-g", "daemon off;"]);
}

#[test]
fn process_config_uses_cmd_when_entrypoint_is_absent() {
    let process = OciProcessConfig::from_image_config(&config_json(serde_json::json!({
        "Cmd": ["/bin/app", "serve"]
    })))
    .expect("parse");
    assert!(process.entrypoint().is_empty());
    assert_eq!(process.argv(), ["/bin/app", "serve"]);
}

#[test]
fn installing_a_service_writes_env_workdir_and_exec_under_services_d() {
    let (_keep, rootfs) = tree_with_services();
    let process = OciProcessConfig::from_image_config(&config_json(serde_json::json!({
        "Entrypoint": ["/bin/app"],
        "Cmd": ["--port", "8080"],
        "Env": ["PATH=/usr/bin", "APP_ENV=prod"],
        "WorkingDir": "/opt/app"
    })))
    .expect("parse");

    install_oci_service(&rootfs, &process).expect("install service");

    let script = std::fs::read_to_string(rootfs.path().join("etc/firecrab/services.d/app"))
        .expect("read service");
    assert!(script.starts_with(&format!("#!{} sh\n", provision::GUEST_TOOLBOX)));
    assert!(script.contains("cd '/opt/app'"));
    assert!(script.contains("export PATH='/usr/bin'"));
    assert!(script.contains("export APP_ENV='prod'"));
    assert!(script.contains("exec '/bin/app' '--port' '8080'"));
    assert!(!script.contains("/sbin/init"));
    assert_eq!(
        std::fs::read(rootfs.path().join("sbin/init")).expect("init untouched"),
        b"injected-init"
    );
}

#[test]
fn the_service_is_executable_and_is_not_pid_1() {
    let (_keep, rootfs) = tree_with_services();
    let process = OciProcessConfig::from_image_config(&config_json(serde_json::json!({
        "Cmd": ["/bin/app"]
    })))
    .expect("parse");

    install_oci_service(&rootfs, &process).expect("install");

    let metadata =
        std::fs::metadata(rootfs.path().join("etc/firecrab/services.d/app")).expect("stat");
    assert!(metadata.permissions().mode() & 0o111 != 0);
    assert_eq!(
        std::fs::read(rootfs.path().join("sbin/init")).expect("init"),
        b"injected-init"
    );
}

#[test]
fn an_image_with_no_command_does_not_install_a_service() {
    let (_keep, rootfs) = tree_with_services();
    let process = OciProcessConfig::from_image_config(&config_json(serde_json::json!({})))
        .expect("empty process");

    install_oci_service(&rootfs, &process).expect("skip empty");

    assert!(!rootfs.path().join("etc/firecrab/services.d/app").exists());
}

#[test]
fn malformed_env_is_refused() {
    let error = OciProcessConfig::from_image_config(&config_json(serde_json::json!({
        "Env": ["PATH=/usr/bin", "NOTAPAIR"]
    })))
    .expect_err("malformed env");
    assert!(matches!(error, ResolveError::ServiceEnvInvalid { .. }));
}

#[test]
fn quotes_in_arguments_stay_inside_the_script() {
    let (_keep, rootfs) = tree_with_services();
    let process = OciProcessConfig::from_image_config(&config_json(serde_json::json!({
        "Entrypoint": ["sh", "-c"],
        "Cmd": ["echo it's"]
    })))
    .expect("parse");

    install_oci_service(&rootfs, &process).expect("install");

    let script = std::fs::read_to_string(rootfs.path().join("etc/firecrab/services.d/app"))
        .expect("read service");
    assert!(script.contains("exec 'sh' '-c' 'echo it'\\''s'"));
}
