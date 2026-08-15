use super::*;

use std::collections::BTreeMap;
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

fn sample_service_script() -> String {
    format!(
        "#!{toolbox} sh\n\
         # Firecrab service for the imported image entrypoint (public-docs/oci.md).\n\
         cd '/opt/app'\n\
         export PATH='/usr/bin'\n\
         export APP_ENV='prod'\n\
         exec '/bin/app' '--port' '8080'\n",
        toolbox = provision::GUEST_TOOLBOX
    )
}

#[test]
fn rewrite_vm_env_block_inserts_after_image_exports_before_exec() {
    let mut env = BTreeMap::new();
    env.insert("FOO".to_owned(), "bar".to_owned());
    let rewritten = service::rewrite_vm_env_block(&sample_service_script(), &env);
    assert!(
        rewritten.contains(
            "export APP_ENV='prod'\n\
             # >>> firecrab vm env\n\
             . /etc/firecrab/vm.env\n\
             # <<< firecrab vm env\n\
             exec '/bin/app' '--port' '8080'\n"
        ),
        "{rewritten}"
    );
    assert!(rewritten.contains("export PATH='/usr/bin'"));
}

#[test]
fn rewrite_vm_env_block_replaces_an_existing_block_in_place() {
    let first = service::rewrite_vm_env_block(
        &sample_service_script(),
        &BTreeMap::from([("FOO".to_owned(), "old".to_owned())]),
    );
    let second = service::rewrite_vm_env_block(
        &first,
        &BTreeMap::from([("FOO".to_owned(), "new".to_owned())]),
    );
    assert!(second.contains(". /etc/firecrab/vm.env"), "{second}");
    assert!(!second.contains("export FOO="), "{second}");
    assert_eq!(
        second.matches("# >>> firecrab vm env").count(),
        1,
        "{second}"
    );
}

#[test]
fn rewrite_vm_env_block_empty_map_removes_markers() {
    let with_block = service::rewrite_vm_env_block(
        &sample_service_script(),
        &BTreeMap::from([("FOO".to_owned(), "bar".to_owned())]),
    );
    let cleared = service::rewrite_vm_env_block(&with_block, &BTreeMap::new());
    assert!(!cleared.contains("firecrab vm env"), "{cleared}");
    assert!(!cleared.contains("export FOO="), "{cleared}");
    assert!(cleared.contains("export PATH='/usr/bin'"));
    assert!(cleared.contains("exec '/bin/app'"));
}

#[test]
fn rewrite_vm_env_block_second_apply_is_byte_identical() {
    let env = BTreeMap::from([
        ("ZED".to_owned(), "z".to_owned()),
        ("ALPHA".to_owned(), "a".to_owned()),
    ]);
    let first = service::rewrite_vm_env_block(&sample_service_script(), &env);
    let second = service::rewrite_vm_env_block(&first, &env);
    assert_eq!(first, second);
    assert!(first.contains(". /etc/firecrab/vm.env"), "{first}");
    let file = service::render_vm_env_file(&env);
    let alpha = file.find("export ALPHA=").expect("ALPHA");
    let zed = file.find("export ZED=").expect("ZED");
    assert!(alpha < zed, "{file}");
}

#[test]
fn posix_quote_escapes_quotes_dollar_and_newline() {
    assert_eq!(service::posix_quote("it's"), "'it'\\''s'");
    assert_eq!(service::posix_quote("$HOME"), "'$HOME'");
    assert_eq!(service::posix_quote("a\nb"), "'a\nb'");
}

#[test]
fn rewrite_vm_env_block_vm_key_wins_after_image_export() {
    let rewritten = service::rewrite_vm_env_block(
        &sample_service_script(),
        &BTreeMap::from([("PATH".to_owned(), "/vm/bin".to_owned())]),
    );
    let image = rewritten
        .find("export PATH='/usr/bin'")
        .expect("image PATH");
    let sourced = rewritten.find(". /etc/firecrab/vm.env").expect("vm env");
    assert!(image < sourced, "{rewritten}");
    assert!(
        service::render_vm_env_file(&BTreeMap::from([("PATH".to_owned(), "/vm/bin".to_owned())]))
            .contains("export PATH='/vm/bin'"),
    );
}

#[test]
fn rewrite_vm_env_block_unclosed_begin_is_left_in_place() {
    let script = "# >>> firecrab vm env\nexport STALE='x'\nexec '/bin/app'\n";
    let rewritten = service::rewrite_vm_env_block(
        script,
        &BTreeMap::from([("FOO".to_owned(), "bar".to_owned())]),
    );
    assert!(
        rewritten.contains("# >>> firecrab vm env\nexport STALE='x'\n"),
        "{rewritten}"
    );
    assert!(
        rewritten.contains(". /etc/firecrab/vm.env\n# <<< firecrab vm env\n"),
        "{rewritten}"
    );
    assert!(rewritten.contains("exec '/bin/app'"), "{rewritten}");
}

#[test]
fn rewrite_vm_env_block_strips_a_block_that_starts_the_script() {
    let script =
        "# >>> firecrab vm env\nexport FOO='bar'\n# <<< firecrab vm env\nexec '/bin/app'\n";
    let cleared = service::rewrite_vm_env_block(script, &BTreeMap::new());
    assert_eq!(cleared, "exec '/bin/app'\n");
}

#[test]
fn rewrite_vm_env_block_inserts_before_exec_when_image_has_no_export() {
    let script = "#!/bin/sh\nexec '/bin/app'\n";
    let rewritten = service::rewrite_vm_env_block(
        script,
        &BTreeMap::from([("FOO".to_owned(), "bar".to_owned())]),
    );
    assert!(
        rewritten.contains(
            "# >>> firecrab vm env\n\
             . /etc/firecrab/vm.env\n\
             # <<< firecrab vm env\n\
             exec '/bin/app'\n"
        ),
        "{rewritten}"
    );
}

#[test]
fn rewrite_vm_env_block_appends_when_script_has_neither_export_nor_exec() {
    let script = "#!/bin/sh\n";
    let rewritten = service::rewrite_vm_env_block(
        script,
        &BTreeMap::from([("FOO".to_owned(), "bar".to_owned())]),
    );
    assert_eq!(
        rewritten,
        "#!/bin/sh\n\
         # >>> firecrab vm env\n\
         . /etc/firecrab/vm.env\n\
         # <<< firecrab vm env\n"
    );
}

#[test]
fn rewrite_vm_env_block_ignores_export_after_exec() {
    let script = "exec '/bin/app'\nexport LATE='x'\n";
    let rewritten = service::rewrite_vm_env_block(
        script,
        &BTreeMap::from([("FOO".to_owned(), "bar".to_owned())]),
    );
    assert!(
        rewritten.starts_with(
            "# >>> firecrab vm env\n\
             . /etc/firecrab/vm.env\n\
             # <<< firecrab vm env\n\
             exec '/bin/app'\n"
        ),
        "{rewritten}"
    );
    assert!(rewritten.contains("export LATE='x'"), "{rewritten}");
}

#[test]
fn rewrite_vm_env_block_recognizes_a_marker_at_eof_without_newline() {
    let script = "# >>> firecrab vm env";
    let rewritten = service::rewrite_vm_env_block(script, &BTreeMap::new());
    assert_eq!(rewritten, script);
}

#[test]
fn rewrite_vm_env_block_value_containing_end_marker_is_byte_identical() {
    let env = BTreeMap::from([(
        "NOTE".to_owned(),
        "keep\n# <<< firecrab vm env\nstill".to_owned(),
    )]);
    let first = service::rewrite_vm_env_block(&sample_service_script(), &env);
    let second = service::rewrite_vm_env_block(&first, &env);
    assert_eq!(first, second);
    assert_eq!(first.matches("# <<< firecrab vm env").count(), 1, "{first}");
    let file = service::render_vm_env_file(&env);
    assert!(file.contains("# <<< firecrab vm env"), "{file}");
    let cleared = service::rewrite_vm_env_block(&first, &BTreeMap::new());
    assert!(!cleared.contains("firecrab vm env"), "{cleared}");
    assert!(cleared.contains("export PATH='/usr/bin'"), "{cleared}");
    assert!(cleared.contains("exec '/bin/app'"), "{cleared}");
}
