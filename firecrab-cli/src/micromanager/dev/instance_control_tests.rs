use super::*;
use crate::micromanager::DevCommand;
use std::net::{Ipv4Addr, TcpListener};

#[test]
fn missing_private_dashboard_settings_reject_start_and_restart_without_changing_state() {
    let directory = tempfile::tempdir().unwrap();
    let instance = test_instance(directory.path(), "0123456789abcdef", 45121, 45122);
    let record = instance.managed_home.join("dev-instance.json");
    let original = fs::read(&record).unwrap();
    for command in [DevCommand::Start, DevCommand::Restart] {
        let error =
            preflight_dashboard(&instance.managed_home, Some(command), 8098, false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("run service dev --port PORT first")
        );
        assert_eq!(fs::read(&record).unwrap(), original);
        assert!(!instance.managed_home.join("runtime").exists());
        assert!(!instance.primary_home.exists());
    }
    for command in [DevCommand::Stop, DevCommand::Clean] {
        preflight_dashboard(&instance.managed_home, Some(command), 8098, false).unwrap();
    }
    preflight_dashboard(&instance.managed_home, None, 8098, true).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    preflight_dashboard(&instance.managed_home, None, port, false).unwrap();
}

#[test]
fn corrupted_private_dashboard_settings_fail_before_control_changes() {
    let directory = tempfile::tempdir().unwrap();
    let instance = test_instance(directory.path(), "0123456789abcdef", 45121, 45122);
    let runtime = instance.managed_home.join("runtime");
    fs::create_dir(&runtime).unwrap();
    let record = runtime.join("dev-dashboard.json");
    fs::write(&record, "invalid dashboard settings").unwrap();
    for command in [
        DevCommand::Start,
        DevCommand::Stop,
        DevCommand::Restart,
        DevCommand::Clean,
    ] {
        assert!(preflight_dashboard(&instance.managed_home, Some(command), 8098, false).is_err());
        assert_eq!(
            fs::read_to_string(&record).unwrap(),
            "invalid dashboard settings"
        );
        assert!(!instance.primary_home.exists());
    }
}
