use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use firecrab_api_types::{
    CreateVmRequest, EgressPolicy, MicroNetworkResponse, PortForward, PortProtocol, VmLogResponse,
    VmResponse, VmState,
};
use serde::Serialize;
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};
use crate::vm_console;

/// How often `vm run` rereads a VM that is still starting.
const START_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Options shared by `vm create` and `vm run`.
#[derive(Debug, clap::Args)]
pub struct CreateArgs {
    /// User-facing MicroVM name.
    #[arg(long)]
    pub name: String,
    /// Installed image alias, such as `alpine-3.24.1`.
    #[arg(long)]
    pub template: String,
    /// Number of virtual CPUs.
    #[arg(long, default_value_t = 1)]
    pub cpu: u8,
    /// Memory in MiB.
    #[arg(long, default_value_t = 512)]
    pub ram: u32,
    /// Disk capacity in GiB.
    #[arg(long, default_value_t = 2)]
    pub disk_gb: u16,
    /// MicroNetwork UUID or name for the VM's ENI-like attachment.
    #[arg(long)]
    pub network: String,
    /// Outbound network posture.
    #[arg(long, value_enum, default_value = "internet")]
    pub egress: EgressArg,
    /// Storage root id; omitted uses the API's default root.
    #[arg(long)]
    pub storage_root: Option<String>,
    /// Existing data volume, repeated as NAME_OR_UUID:/guest/path.
    #[arg(long, value_name = "VOLUME:/PATH")]
    pub volume: Vec<String>,
    /// Host port forwarded to a guest port, repeated as HOST:GUEST[/tcp|udp].
    #[arg(short = 'p', long, value_name = "HOST:GUEST[/PROTO]", value_parser = parse_publish)]
    pub publish: Vec<PortForward>,
    /// Environment variable for the image's app, repeated as NAME=VALUE.
    #[arg(short = 'e', long, value_name = "NAME=VALUE", value_parser = parse_env)]
    pub env: Vec<(String, String)>,
    /// Emit the resulting VM as JSON.
    #[arg(long)]
    pub json: bool,
}

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
    Create(CreateArgs),
    /// Create a MicroVM and start it in one step.
    Run {
        #[command(flatten)]
        create: CreateArgs,
        /// Return after the start request instead of waiting for `running`.
        #[arg(long)]
        no_wait: bool,
        /// Seconds to wait for `running`; reaching it does not stop the VM.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Start an inactive MicroVM.
    Start {
        /// MicroVM UUID or name.
        #[arg(value_name = "VM")]
        target: String,
        /// Emit the resulting VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Pause guest execution while keeping its memory allocated.
    Pause {
        /// MicroVM UUID or name.
        #[arg(value_name = "VM")]
        target: String,
        /// Emit the resulting VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resume a paused MicroVM without rebooting it.
    Resume {
        /// MicroVM UUID or name.
        #[arg(value_name = "VM")]
        target: String,
        /// Emit the resulting VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop a running MicroVM.
    Stop {
        /// MicroVM UUID or name.
        #[arg(value_name = "VM")]
        target: String,
        /// Emit the resulting VM as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete an inactive MicroVM and its disk.
    Delete {
        /// MicroVM UUID or name.
        #[arg(value_name = "VM")]
        target: String,
        /// Emit a deletion acknowledgement as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print a MicroVM's console transcript, including its app output.
    Logs {
        /// MicroVM UUID or name.
        #[arg(value_name = "VM")]
        target: String,
        /// Print only the last lines of the transcript.
        #[arg(long, value_name = "LINES")]
        tail: Option<usize>,
        /// Emit the API response as JSON.
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
    /// Name lookup found no matching MicroNetwork.
    #[error("no MicroNetwork named {0:?}")]
    NetworkNotFound(String),
    /// Several MicroNetworks share the name, so only a UUID selects one.
    #[error("multiple MicroNetworks named {0:?}: pass a UUID")]
    AmbiguousNetwork(String),
    /// `vm run` created the VM but the start request failed.
    #[error("created VM {id} but could not start it: {source}")]
    StartAfterCreate {
        id: Uuid,
        #[source]
        source: ApiError,
    },
    /// A started VM left the startup path without reaching `running`.
    #[error("VM {id} is {state} instead of running; read `firecrab vm logs {id}`")]
    NotRunning { id: Uuid, state: &'static str },
    /// The waiting budget ended while the VM was still starting.
    #[error(
        "VM {id} was still {state} after {seconds} seconds and keeps starting; check `firecrab vm list`"
    )]
    StartTimeout {
        id: Uuid,
        state: &'static str,
        seconds: u64,
    },
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
        Command::Create(args) => {
            let json = args.json;
            let vm = create_vm(client, args)?;
            print_output(json, &vm, format_vm_human(&vm));
        }
        Command::Run {
            create,
            no_wait,
            timeout,
        } => {
            let json = create.json;
            let created = create_vm(client, create)?;
            let started: VmResponse = client
                .post_empty(&format!("/api/vms/{}/start", created.id))
                .map_err(|source| Error::StartAfterCreate {
                    id: created.id,
                    source,
                })?;
            let vm = if no_wait {
                started
            } else {
                wait_until_running(client, started, Duration::from_secs(timeout))?
            };
            print_output(json, &vm, format_vm_human(&vm));
        }
        Command::Start { target, json } => change_state(client, &target, "start", json)?,
        Command::Pause { target, json } => change_state(client, &target, "pause", json)?,
        Command::Resume { target, json } => change_state(client, &target, "resume", json)?,
        Command::Stop { target, json } => change_state(client, &target, "stop", json)?,
        Command::Delete { target, json } => {
            let id = resolve_vm_target(client, &target)?;
            client.delete(&format!("/api/vms/{id}"))?;
            if json {
                println!("{}", format_json(&serde_json::json!({ "deleted": id })));
            } else {
                println!("deleted VM {id}");
            }
        }
        Command::Logs { target, tail, json } => {
            let id = resolve_vm_target(client, &target)?;
            let log: VmLogResponse = client.get(&format!("/api/vms/{id}/log"))?;
            if json {
                println!("{}", format_json(&log));
            } else {
                let text = match tail {
                    Some(lines) => tail_lines(&log.console_log, lines),
                    None => &log.console_log,
                };
                print!("{text}");
                if !text.is_empty() && !text.ends_with('\n') {
                    println!();
                }
                if log.truncated {
                    eprintln!(
                        "note: the API returned only the first part of a longer console log; `firecrab vm console` shows live output"
                    );
                }
            }
        }
        Command::Console { target } => {
            let id = resolve_vm_target(client, &target)?;
            vm_console::attach(client.base_url(), id)?;
        }
    }
    Ok(())
}

/// Sends one lifecycle action to the VM that `target` names.
fn change_state(client: &ApiClient, target: &str, action: &str, json: bool) -> Result<(), Error> {
    let id = resolve_vm_target(client, target)?;
    let vm: VmResponse = client.post_empty(&format!("/api/vms/{id}/{action}"))?;
    print_output(json, &vm, format_vm_human(&vm));
    Ok(())
}

/// Resolves the network and volumes, then creates the VM without starting it.
fn create_vm(client: &ApiClient, args: CreateArgs) -> Result<VmResponse, Error> {
    let request = CreateVmRequest {
        micro_network_id: resolve_network(client, &args.network)?,
        block_storages: crate::volume::resolve_mounts(client, &args.volume)?,
        security_group_id: None,
        name: args.name,
        template: args.template,
        ram: args.ram,
        cpu: args.cpu,
        disk_gb: args.disk_gb,
        egress_policy: args.egress.into(),
        storage_root: args.storage_root,
        shell_ids: Vec::new(),
        port_forwards: args.publish,
        // A repeated name keeps its last value, as in a shell environment.
        env: args.env.into_iter().collect::<BTreeMap<_, _>>(),
    };
    Ok(client.post("/api/vms", &request)?)
}

/// Rereads a started VM until it runs, fails, or `budget` is spent.
fn wait_until_running(
    client: &ApiClient,
    mut vm: VmResponse,
    budget: Duration,
) -> Result<VmResponse, Error> {
    let deadline = Instant::now() + budget;
    loop {
        match vm.state {
            VmState::Running => return Ok(vm),
            VmState::Created | VmState::Starting => {}
            state => {
                return Err(Error::NotRunning {
                    id: vm.id,
                    state: state_name(state),
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::StartTimeout {
                id: vm.id,
                state: state_name(vm.state),
                seconds: budget.as_secs(),
            });
        }
        std::thread::sleep(START_POLL_INTERVAL);
        vm = client.get(&format!("/api/vms/{}", vm.id))?;
    }
}

/// Parses `HOST_PORT:GUEST_PORT[/tcp|udp]`; the protocol defaults to TCP.
fn parse_publish(value: &str) -> Result<PortForward, String> {
    let (ports, protocol) = match value.split_once('/') {
        Some((ports, "tcp")) => (ports, PortProtocol::Tcp),
        Some((ports, "udp")) => (ports, PortProtocol::Udp),
        Some((_, other)) => return Err(format!("unknown protocol {other:?}; use tcp or udp")),
        None => (value, PortProtocol::Tcp),
    };
    let port = |text: &str| text.parse::<u16>().ok().filter(|port| *port != 0);
    match ports
        .split_once(':')
        .map(|(host, guest)| (port(host), port(guest)))
    {
        Some((Some(host_port), Some(guest_port))) => Ok(PortForward {
            host_port,
            guest_port,
            protocol,
        }),
        _ => Err("expected HOST_PORT:GUEST_PORT[/tcp|udp] with ports from 1 to 65535".to_owned()),
    }
}

/// Parses `NAME=VALUE`, splitting at the first `=` so values may contain it.
fn parse_env(value: &str) -> Result<(String, String), String> {
    match value.split_once('=') {
        Some((name, text)) if !name.is_empty() => Ok((name.to_owned(), text.to_owned())),
        _ => Err("expected NAME=VALUE".to_owned()),
    }
}

/// Returns the last `count` lines of `text`, keeping its final newline.
fn tail_lines(text: &str, count: usize) -> &str {
    let Some(skipped_breaks) = count.checked_sub(1) else {
        return "";
    };
    let body = text.strip_suffix('\n').unwrap_or(text);
    match body.rmatch_indices('\n').nth(skipped_breaks) {
        Some((line_break, _)) => &text[line_break + 1..],
        None => text,
    }
}

/// Resolves a MicroNetwork UUID or unique name to its UUID.
///
/// A UUID is returned without contacting the API, as in [`resolve_vm_target`].
fn resolve_network(client: &ApiClient, target: &str) -> Result<Uuid, Error> {
    if let Ok(uuid) = Uuid::parse_str(target) {
        return Ok(uuid);
    }
    let networks: Vec<MicroNetworkResponse> = client.get("/api/micro-networks")?;
    let mut matches = networks
        .into_iter()
        .filter(|network| network.name == target);
    match (matches.next(), matches.next()) {
        (Some(network), None) => Ok(network.id),
        (None, _) => Err(Error::NetworkNotFound(target.to_owned())),
        (Some(_), Some(_)) => Err(Error::AmbiguousNetwork(target.to_owned())),
    }
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
        VmState::Pausing => "pausing",
        VmState::Paused => "paused",
        VmState::Resuming => "resuming",
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
            security_group_id: None,
            id: id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            name: "web-1".to_owned(),
            purpose: firecrab_api_types::VmPurpose::Instance,
            state: VmState::Running,
            reconciliation: None,
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
            Command::Create(CreateArgs {
                name,
                template,
                cpu,
                ram,
                disk_gb,
                network,
                egress,
                storage_root,
                volume,
                publish,
                env,
                json,
            }) => {
                assert_eq!(name, "web-1");
                assert_eq!(template, "alpine-3.24.1");
                assert_eq!((cpu, ram, disk_gb), (1, 512, 2));
                assert_eq!(network, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
                assert_eq!(egress, EgressArg::Internet);
                assert_eq!(storage_root, None);
                assert!(volume.is_empty());
                assert!(publish.is_empty());
                assert!(env.is_empty());
                assert!(!json);
            }
            _ => panic!("expected create command"),
        }
    }

    #[test]
    fn publish_defaults_to_tcp_and_accepts_udp() {
        assert_eq!(
            parse_publish("8080:80"),
            Ok(PortForward {
                host_port: 8080,
                guest_port: 80,
                protocol: PortProtocol::Tcp,
            })
        );
        assert_eq!(
            parse_publish("5353:53/udp"),
            Ok(PortForward {
                host_port: 5353,
                guest_port: 53,
                protocol: PortProtocol::Udp,
            })
        );
        assert_eq!(
            parse_publish("65535:1/tcp").map(|rule| (rule.host_port, rule.guest_port)),
            Ok((65535, 1))
        );
    }

    #[test]
    fn publish_rejects_missing_zero_and_out_of_range_ports() {
        for value in [
            "80", "8080:", ":80", "0:80", "8080:0", "65536:80", "a:b", "1:2:3", "",
        ] {
            assert!(parse_publish(value).is_err(), "{value:?} must be rejected");
        }
        assert_eq!(
            parse_publish("8080:80/sctp").unwrap_err(),
            "unknown protocol \"sctp\"; use tcp or udp"
        );
    }

    #[test]
    fn env_splits_at_the_first_equals_sign() {
        assert_eq!(parse_env("A=1"), Ok(("A".to_owned(), "1".to_owned())));
        assert_eq!(
            parse_env("URL=http://a.test/?b=c"),
            Ok(("URL".to_owned(), "http://a.test/?b=c".to_owned()))
        );
        assert_eq!(parse_env("EMPTY="), Ok(("EMPTY".to_owned(), String::new())));
    }

    #[test]
    fn env_rejects_a_missing_name_or_separator() {
        for value in ["NAME", "=value", ""] {
            assert!(parse_env(value).is_err(), "{value:?} must be rejected");
        }
    }

    #[test]
    fn tail_keeps_only_the_last_lines() {
        assert_eq!(tail_lines("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail_lines("a\nb\nc\n", 1), "c\n");
        assert_eq!(tail_lines("a\nb\nc", 1), "c");
        assert_eq!(tail_lines("a\nb\nc\n", 3), "a\nb\nc\n");
        assert_eq!(tail_lines("a\nb\nc\n", 9), "a\nb\nc\n");
        assert_eq!(tail_lines("a\nb\nc\n", 0), "");
        assert_eq!(tail_lines("", 4), "");
    }

    #[test]
    fn a_network_uuid_resolves_without_contacting_the_api() {
        let client = ApiClient::new("http://127.0.0.1:1".to_owned());
        let network = id("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
        assert_eq!(
            resolve_network(&client, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap(),
            network
        );
        assert!(matches!(
            resolve_network(&client, "lab"),
            Err(Error::Api(_))
        ));
    }

    #[test]
    fn run_shares_create_options_and_adds_waiting_controls() {
        let cli = TestCli::try_parse_from([
            "test",
            "run",
            "--name",
            "web-1",
            "--template",
            "alpine-3.24.1",
            "--network",
            "lab",
            "-p",
            "8080:80",
        ])
        .unwrap();
        match cli.command {
            Command::Run {
                create,
                no_wait,
                timeout,
            } => {
                assert_eq!(create.network, "lab");
                assert_eq!(create.publish.len(), 1);
                assert!(!no_wait);
                assert_eq!(timeout, 120);
            }
            _ => panic!("expected run command"),
        }

        let cli = TestCli::try_parse_from([
            "test",
            "run",
            "--name",
            "web-1",
            "--template",
            "alpine-3.24.1",
            "--network",
            "lab",
            "--no-wait",
            "--timeout",
            "30",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Run {
                no_wait: true,
                timeout: 30,
                ..
            }
        ));
    }

    #[test]
    fn a_failed_start_after_create_names_the_created_vm() {
        let error = Error::StartAfterCreate {
            id: id("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            source: ApiError::Http {
                status: 409,
                body: "host port 8080 is in use".to_owned(),
            },
        };
        let text = error.to_string();
        assert!(
            text.starts_with("created VM aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa but could not start"),
            "{text}"
        );
        assert!(text.contains("host port 8080 is in use"), "{text}");
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
            Command::Create(CreateArgs {
                cpu,
                ram,
                disk_gb,
                egress,
                storage_root,
                json,
                ..
            }) => {
                assert_eq!((cpu, ram, disk_gb), (4, 2048, 20));
                assert_eq!(egress, EgressArg::Isolated);
                assert_eq!(storage_root.as_deref(), Some("fast"));
                assert!(json);
            }
            _ => panic!("expected create command"),
        }
    }

    #[test]
    fn lifecycle_commands_accept_a_uuid_or_a_name() {
        for target in ["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "web-1"] {
            for action in ["start", "pause", "resume", "stop", "delete", "logs"] {
                let cli = TestCli::try_parse_from(["test", action, target, "--json"]).unwrap();
                let (parsed, json) = match cli.command {
                    Command::Start { target, json }
                    | Command::Pause { target, json }
                    | Command::Resume { target, json }
                    | Command::Stop { target, json }
                    | Command::Delete { target, json }
                    | Command::Logs { target, json, .. } => (target, json),
                    other => panic!("unexpected command for {action}: {other:?}"),
                };
                assert_eq!(parsed, target, "vm {action}");
                assert!(json, "vm {action} --json");
            }
        }
        assert!(TestCli::try_parse_from(["test", "start"]).is_err());
        assert_eq!(state_name(VmState::Paused), "paused");
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
