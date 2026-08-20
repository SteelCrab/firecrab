use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use firecrab_bench::{
    HttpVmApi, VmSpec, run_boot, run_concurrent_creation, run_density, run_lifecycle,
};
use uuid::Uuid;

const DEFAULT_API_BASE: &str = "http://127.0.0.1:5523";

/// Firecrab MicroVM benchmark runner.
#[derive(Debug, Parser)]
#[command(name = "firecrab-bench", version, about)]
struct Cli {
    /// Firecrab API base URL. Defaults to FIRECRAB_API, then localhost.
    #[arg(long, global = true)]
    api: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Measure sequential create-request through running-state latency.
    Boot(BootArgs),
    /// Measure a concurrent group of VM creations and boots.
    Create(CreateArgs),
    /// Find the maximum VM count that remains in running state.
    Density(DensityArgs),
    /// Repeat create/start/stop/start/stop/delete lifecycles.
    Lifecycle(LifecycleArgs),
}

#[derive(Debug, Args)]
struct VmArgs {
    /// Registered template alias used for every VM.
    #[arg(long)]
    template: String,
    /// MicroNetwork UUID used for every VM.
    #[arg(long)]
    micro_network_id: Uuid,
    /// Guest RAM in MiB.
    #[arg(long, default_value_t = 512)]
    ram: u32,
    /// Guest vCPU count.
    #[arg(long, default_value_t = 1)]
    cpu: u8,
    /// Guest disk capacity in GiB.
    #[arg(long, default_value_t = 8)]
    disk_gb: u16,
}

impl From<VmArgs> for VmSpec {
    fn from(args: VmArgs) -> Self {
        Self {
            template: args.template,
            micro_network_id: args.micro_network_id,
            ram: args.ram,
            cpu: args.cpu,
            disk_gb: args.disk_gb,
        }
    }
}

#[derive(Debug, Args)]
struct BootArgs {
    /// Number of sequential VM boots to measure.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    count: u32,
    #[command(flatten)]
    vm: VmArgs,
}

#[derive(Debug, Args)]
struct CreateArgs {
    /// Number of VMs created and booted at the same time.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    concurrency: u32,
    #[command(flatten)]
    vm: VmArgs,
}

#[derive(Debug, Args)]
struct DensityArgs {
    /// Upper bound for running VMs on this host.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    max_vms: u32,
    /// Number of VMs added during each density step.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..))]
    step: u32,
    /// Stability observation time after each step.
    #[arg(long, default_value_t = 5)]
    settle_seconds: u64,
    #[command(flatten)]
    vm: VmArgs,
}

#[derive(Debug, Args)]
struct LifecycleArgs {
    /// Number of complete lifecycle repetitions.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    iterations: u32,
    #[command(flatten)]
    vm: VmArgs,
}

fn main() {
    let cli = Cli::parse();
    let api = HttpVmApi::new(resolve_api_base(cli.api.as_deref()));
    let result = match cli.command {
        Command::Boot(args) => run_boot(&api, &args.vm.into(), args.count),
        Command::Create(args) => run_concurrent_creation(&api, &args.vm.into(), args.concurrency),
        Command::Density(args) => run_density(
            &api,
            &args.vm.into(),
            args.max_vms,
            args.step,
            Duration::from_secs(args.settle_seconds),
        ),
        Command::Lifecycle(args) => run_lifecycle(&api, &args.vm.into(), args.iterations),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("result serialization")
    );
    if result.failed_count > 0 {
        std::process::exit(1);
    }
}

fn resolve_api_base(flag: Option<&str>) -> String {
    flag.map(str::to_owned)
        .or_else(|| {
            std::env::var("FIRECRAB_API")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_API_BASE.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_flags(network: Uuid) -> Vec<String> {
        vec![
            "--template".to_owned(),
            "ubuntu".to_owned(),
            "--micro-network-id".to_owned(),
            network.to_string(),
        ]
    }

    #[test]
    fn cli_parses_all_core_commands() {
        let network = Uuid::new_v4();
        for (command, option, value) in [
            ("boot", "--count", "5"),
            ("create", "--concurrency", "10"),
            ("density", "--max-vms", "20"),
            ("lifecycle", "--iterations", "30"),
        ] {
            let mut arguments = vec!["firecrab-bench".to_owned(), command.to_owned()];
            arguments.extend([option.to_owned(), value.to_owned()]);
            arguments.extend(vm_flags(network));
            assert!(
                Cli::try_parse_from(arguments).is_ok(),
                "failed to parse {command}"
            );
        }
    }

    #[test]
    fn density_parses_step_and_settle_time() {
        let network = Uuid::new_v4();
        let mut arguments = vec![
            "firecrab-bench".to_owned(),
            "density".to_owned(),
            "--max-vms".to_owned(),
            "20".to_owned(),
            "--step".to_owned(),
            "5".to_owned(),
            "--settle-seconds".to_owned(),
            "1".to_owned(),
        ];
        arguments.extend(vm_flags(network));
        let cli = Cli::try_parse_from(arguments).unwrap();
        assert!(matches!(
            cli.command,
            Command::Density(DensityArgs {
                max_vms: 20,
                step: 5,
                settle_seconds: 1,
                ..
            })
        ));
    }

    #[test]
    fn api_base_strips_a_trailing_slash() {
        assert_eq!(
            resolve_api_base(Some("http://example.test/")),
            "http://example.test"
        );
    }
}
