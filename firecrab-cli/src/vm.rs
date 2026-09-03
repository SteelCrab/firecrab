use std::collections::BTreeMap;
use std::fmt::Write;

use clap::ValueEnum;
use firecrab_api_types::{CreateVmRequest, EgressPolicy, VmResponse, VmState};
use serde::Serialize;
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};
use crate::vm_console;

/// MicroVM operations backed by firecrab-api.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// List every MicroVM on the host.
    List {
        /// Emit the API response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a MicroVM from an installed image.
    Create {
        /// User-facing MicroVM name.
        #[arg(long)]
        name: String,
        /// Installed image alias, such as `alpine-3.24.1`.
        #[arg(long)]
        template: String,
        /// Number of virtual CPUs.
        #[arg(long, default_value_t = 1)]
        cpu: u8,
        /// Memory in MiB.
        #[arg(long, default_value_t = 512)]
        ram: u32,
        /// Disk capacity in GiB.
        #[arg(long, default_value_t = 2)]
        disk_gb: u16,
        /// MicroNetwork UUID for the VM's ENI-like attachment.
        #[arg(long)]
        network: Uuid,
        /// Outbound network posture.
        #[arg(long, value_enum, default_value = "internet")]
        egress: EgressArg,
        /// Storage root id; omitted uses the API's default root.
        #[arg(long)]
        storage_root: Option<String>,
        /// Emit the created VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start an inactive MicroVM.
    Start {
        /// MicroVM UUID.
        id: Uuid,
        /// Emit the resulting VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop a running MicroVM.
    Stop {
        /// MicroVM UUID.
        id: Uuid,
        /// Emit the resulting VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete an inactive MicroVM and its disk.
    Delete {
        /// MicroVM UUID.
        id: Uuid,
        /// Emit a deletion acknowledgement as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Attach to a running MicroVM's serial console.
    #[command(visible_alias = "terminal")]
    Console {
        /// MicroVM UUID or name.
        target: String,
    },
}

/// Errors produced while executing a VM command.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// REST API operation failure.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// Interactive serial-console failure.
    #[error(transparent)]
    Console(#[from] vm_console::Error),
    /// Name lookup found no matching VM.
    #[error("no VM named {0:?}")]
    VmNotFound(String),
    /// Multiple VMs share the name and stdin is not a TTY so interactive
    /// selection is not possible.
    #[error("multiple VMs named {0:?}: pipe a UUID or run interactively")]
    AmbiguousNonInteractive(String),
    /// The user entered a selection that is out of range.
    #[error("selection {0} is out of range")]
    InvalidSelection(usize),
    /// Reading the selection from stdin failed.
    #[error("could not read selection: {0}")]
    SelectionIo(#[from] std::io::Error),
}

/// CLI spelling of [`EgressPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EgressArg {
    /// Permit outbound internet access.
    Internet,
    /// Permit only gateway-local services such as DHCP and DNS.
    Isolated,
}

impl From<EgressArg> for EgressPolicy {
    fn from(value: EgressArg) -> Self {
        match value {
            EgressArg::Internet => Self::Internet,
            EgressArg::Isolated => Self::Isolated,
        }
    }
}

/// Executes one `firecrab vm` command through the loopback REST API.
pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::List { json } => {
            let vms: Vec<VmResponse> = client.get("/api/vms")?;
            print_output(json, &vms, format_list_human(&vms));
        }
        Command::Create {
            name,
            template,
            cpu,
            ram,
            disk_gb,
            network,
            egress,
            storage_root,
            json,
        } => {
            let request = CreateVmRequest {
                name,
                template,
                ram,
                cpu,
                disk_gb,
                egress_policy: egress.into(),
                micro_network_id: network,
                storage_root,
                shell_ids: Vec::new(),
                port_forwards: Vec::new(),
                env: BTreeMap::new(),
            };
            let vm: VmResponse = client.post("/api/vms", &request)?;
            print_output(json, &vm, format_vm_human(&vm));
        }
        Command::Start { id, json } => {
            let path = format!("/api/vms/{id}/start");
            let vm: VmResponse = client.post_empty(&path)?;
            print_output(json, &vm, format_vm_human(&vm));
        }
        Command::Stop { id, json } => {
            let path = format!("/api/vms/{id}/stop");
            let vm: VmResponse = client.post_empty(&path)?;
            print_output(json, &vm, format_vm_human(&vm));
        }
        Command::Delete { id, json } => {
            client.delete(&format!("/api/vms/{id}"))?;
            if json {
                println!("{}", format_json(&serde_json::json!({ "deleted": id })));
            } else {
                println!("deleted VM {id}");
            }
        }
        Command::Console { target } => {
            let id = resolve_vm_target(client, &target)?;
            vm_console::attach(client.base_url(), id)?;
        }
    }
    Ok(())
}

/// Resolves a `target` string (UUID or VM name) to a VM UUID.
///
/// If `target` parses as a [`Uuid`] it is returned as-is — no network
/// round-trip. Otherwise the API VM list is fetched and all VMs whose `.name`
/// matches `target` are collected:
/// - exactly one match → return its UUID
/// - zero matches → [`Error::VmNotFound`]
/// - two or more matches → interactive numbered selection on stderr/stdin
///   (non-TTY stdin → [`Error::AmbiguousNonInteractive`])
fn resolve_vm_target(client: &ApiClient, target: &str) -> Result<Uuid, Error> {
    if let Ok(uuid) = Uuid::parse_str(target) {
        return Ok(uuid);
    }
    let vms: Vec<VmResponse> = client.get("/api/vms")?;
    let matches: Vec<VmResponse> = vms.into_iter().filter(|vm| vm.name == target).collect();
    match matches.len() {
        0 => Err(Error::VmNotFound(target.to_owned())),
        1 => Ok(matches.into_iter().next().unwrap().id),
        _ => select_from_matches(target, matches),
    }
}

/// Prints a numbered list of matching VMs to stderr and reads the user's
/// choice from stdin. Falls back to [`Error::AmbiguousNonInteractive`] when
/// stdin is not a TTY (script / pipe context).
fn select_from_matches(name: &str, candidates: Vec<VmResponse>) -> Result<Uuid, Error> {
    use std::io::IsTerminal;

    if !std::io::stdin().is_terminal() {
        return Err(Error::AmbiguousNonInteractive(name.to_owned()));
    }

    eprintln!("Multiple VMs named {name:?} — select one:");
    for (i, vm) in candidates.iter().enumerate() {
        eprintln!(
            "  [{n}] {id}  {state:<8}  {ip}",
            n = i + 1,
            id = vm.id,
            state = state_name(vm.state),
            ip = vm.ipv4.as_deref().unwrap_or("-"),
        );
    }
    eprint!("Enter number [1-{}]: ", candidates.len());

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let choice: usize = line.trim().parse().unwrap_or(0);
    if choice == 0 || choice > candidates.len() {
        return Err(Error::InvalidSelection(choice));
    }
    Ok(candidates.into_iter().nth(choice - 1).unwrap().id)
}

fn print_output<T: Serialize>(json: bool, value: &T, human: String) {
    if json {
        println!("{}", format_json(value));
    } else {
        print!("{human}");
    }
}

fn format_json<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("CLI output serializes")
}

fn format_list_human(vms: &[VmResponse]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "ID\tNAME\tSTATE\tTEMPLATE\tVCPU\tRAM_MIB\tDISK_GIB\tIPV4"
    )
    .unwrap();
    for vm in vms {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            vm.id,
            vm.name,
            state_name(vm.state),
            vm.template,
            vm.cpu,
            vm.ram,
            vm.disk_gb,
            vm.ipv4.as_deref().unwrap_or("-")
        )
        .unwrap();
    }
    out
}

fn format_vm_human(vm: &VmResponse) -> String {
    let mut out = String::new();
    writeln!(out, "VM {}", vm.id).unwrap();
    writeln!(out, "  name:      {}", vm.name).unwrap();
    writeln!(out, "  state:     {}", state_name(vm.state)).unwrap();
    writeln!(out, "  template:  {}@{}", vm.template, vm.template_version).unwrap();
    writeln!(
        out,
        "  resources: {} vCPU, {} MiB RAM, {} GiB disk",
        vm.cpu, vm.ram, vm.disk_gb
    )
    .unwrap();
    writeln!(out, "  network:   {}", vm.micro_network_id).unwrap();
    writeln!(out, "  ipv4:      {}", vm.ipv4.as_deref().unwrap_or("-")).unwrap();
    writeln!(out, "  egress:    {}", vm.egress_policy).unwrap();
    writeln!(out, "  storage:   {}", vm.storage_root).unwrap();
    out
}

fn state_name(state: VmState) -> &'static str {
    match state {
        VmState::Created => "created",
        VmState::Starting => "starting",
        VmState::Running => "running",
        VmState::Stopping => "stopping",
        VmState::Stopped => "stopped",
        VmState::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::io::IsTerminal;

    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    fn id(value: &str) -> Uuid {
        Uuid::parse_str(value).unwrap()
    }

    fn sample_vm() -> VmResponse {
        VmResponse {
            id: id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            name: "web-1".to_owned(),
            state: VmState::Running,
            template: "alpine-3.24.1".to_owned(),
            template_version: "3.24.1".to_owned(),
            cpu: 2,
            ram: 1024,
            disk_gb: 4,
            startup_step: None,
            egress_policy: EgressPolicy::Internet,
            ipv4: Some("172.31.0.10".to_owned()),
            ipv6: None,
            mac: Some("02:fc:00:00:00:01".to_owned()),
            hostname: "fc-aaaaaaaaaaaa".to_owned(),
            startup_timeline: Vec::new(),
            micro_network_id: id("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
            storage_root: "default".to_owned(),
            cpu_usage_percent: None,
            memory_used_mib: None,
            memory_total_mib: None,
            memory_used_percent: None,
            usage_history: Vec::new(),
            shell_refs: Vec::new(),
            port_forwards: Vec::new(),
            env: BTreeMap::new(),
            ssh_host_fingerprint: None,
        }
    }

    #[test]
    fn create_parses_defaults_and_required_network() {
        let cli = TestCli::try_parse_from([
            "test",
            "create",
            "--name",
            "web-1",
            "--template",
            "alpine-3.24.1",
            "--network",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        ])
        .unwrap();

        match cli.command {
            Command::Create {
                name,
                template,
                cpu,
                ram,
                disk_gb,
                network,
                egress,
                storage_root,
                json,
            } => {
                assert_eq!(name, "web-1");
                assert_eq!(template, "alpine-3.24.1");
                assert_eq!((cpu, ram, disk_gb), (1, 512, 2));
                assert_eq!(network, id("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"));
                assert_eq!(egress, EgressArg::Internet);
                assert_eq!(storage_root, None);
                assert!(!json);
            }
            _ => panic!("expected create command"),
        }
    }

    #[test]
    fn create_parses_resource_and_policy_options() {
        let cli = TestCli::try_parse_from([
            "test",
            "create",
            "--name",
            "db-1",
            "--template",
            "rocky-9.8",
            "--cpu",
            "4",
            "--ram",
            "2048",
            "--disk-gb",
            "20",
            "--network",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "--egress",
            "isolated",
            "--storage-root",
            "fast",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Command::Create {
                cpu,
                ram,
                disk_gb,
                egress,
                storage_root,
                json,
                ..
            } => {
                assert_eq!((cpu, ram, disk_gb), (4, 2048, 20));
                assert_eq!(egress, EgressArg::Isolated);
                assert_eq!(storage_root.as_deref(), Some("fast"));
                assert!(json);
            }
            _ => panic!("expected create command"),
        }
    }

    #[test]
    fn lifecycle_commands_require_uuid_ids() {
        assert!(TestCli::try_parse_from(["test", "start", "not-a-uuid"]).is_err());
        let cli = TestCli::try_parse_from([
            "test",
            "stop",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--json",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::Stop { json: true, .. }));
    }

    #[test]
    fn console_accepts_uuid_name_and_terminal_alias() {
        for subcmd in ["console", "terminal"] {
            // UUID target
            let cli =
                TestCli::try_parse_from(["test", subcmd, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"])
                    .unwrap();
            assert!(matches!(
                cli.command,
                Command::Console { ref target } if target == "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
            ));

            // VM name target
            let cli = TestCli::try_parse_from(["test", subcmd, "web-1"]).unwrap();
            assert!(matches!(
                cli.command,
                Command::Console { ref target } if target == "web-1"
            ));
        }
        // Missing target is still an error
        assert!(TestCli::try_parse_from(["test", "console"]).is_err());
    }

    #[test]
    fn resolve_vm_target_parses_uuid_without_api_call() {
        // resolve_vm_target must not reach the network when given a valid UUID —
        // verified implicitly: ApiClient on port 1 is unreachable, yet no error.
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let uuid = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let result = resolve_vm_target(&client, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert_eq!(result.unwrap(), uuid);
    }

    #[test]
    fn resolve_vm_target_returns_not_found_on_unreachable_api_for_name() {
        // A name (not a UUID) triggers a GET /api/vms call; unreachable API → Api error.
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let result = resolve_vm_target(&client, "web-1");
        assert!(matches!(result, Err(Error::Api(_))));
    }

    #[test]
    fn select_from_matches_non_interactive_returns_ambiguous_error() {
        // In a non-TTY test environment stdin is not a terminal, so
        // select_from_matches must return AmbiguousNonInteractive immediately.
        if std::io::stdin().is_terminal() {
            return; // skip when running in an interactive shell
        }
        let candidates = vec![
            {
                let mut vm = sample_vm();
                vm.id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
                vm
            },
            {
                let mut vm = sample_vm();
                vm.id = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
                vm
            },
        ];
        let result = select_from_matches("web-1", candidates);
        assert!(matches!(result, Err(Error::AmbiguousNonInteractive(_))));
    }

    #[test]
    fn list_human_has_a_header_and_vm_row() {
        let text = format_list_human(&[sample_vm()]);
        assert!(text.starts_with("ID\tNAME\tSTATE\tTEMPLATE\t"), "{text}");
        assert!(
            text.contains("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\tweb-1\trunning\talpine-3.24.1"),
            "{text}"
        );
        assert!(text.contains("\t2\t1024\t4\t172.31.0.10\n"), "{text}");
    }

    #[test]
    fn single_vm_human_includes_core_fields() {
        let text = format_vm_human(&sample_vm());
        assert!(text.contains("VM aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"));
        assert!(text.contains("state:     running"));
        assert!(text.contains("template:  alpine-3.24.1@3.24.1"));
        assert!(text.contains("resources: 2 vCPU, 1024 MiB RAM, 4 GiB disk"));
        assert!(text.contains("network:   bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"));
        assert!(text.contains("egress:    internet"));
    }

    #[test]
    fn list_json_is_pretty_and_preserves_api_field_names() {
        let json = format_json(&[sample_vm()]);
        assert!(json.contains("\n  {"), "{json}");
        assert!(json.contains("\"diskGb\": 4"), "{json}");
        assert!(json.contains("\"microNetworkId\""), "{json}");
    }

    #[test]
    fn delete_json_is_a_pretty_receipt() {
        let vm_id = id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        let json = format_json(&serde_json::json!({ "deleted": vm_id }));
        assert_eq!(
            json,
            "{\n  \"deleted\": \"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\"\n}"
        );
    }
}
