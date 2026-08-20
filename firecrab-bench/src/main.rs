use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use firecrab_bench::{
    HttpVmApi, NetworkConfig, StorageConfig, StorageMode, VmSpec, publish_result, run_api_load,
    run_boot, run_concurrent_creation, run_density, run_leak_check, run_lifecycle, run_network,
    run_regression_files, run_soak, run_storage,
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
    /// Write the common JSON result to this file.
    #[arg(long, global = true)]
    output: Option<PathBuf>,
    /// Publish the completed result to the Firecrab Benchmark API.
    #[arg(long, global = true)]
    publish: bool,
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
    /// Send concurrent read-only requests to a Firecrab API path.
    Api(ApiArgs),
    /// Run an iperf3 benchmark against a prepared server.
    Network(NetworkArgs),
    /// Run fio against a temporary file in an explicit directory.
    Storage(StorageArgs),
    /// Repeat VM lifecycles for a bounded duration.
    Soak(SoakArgs),
    /// Detect positive host resource deltas after repeated lifecycles.
    Leak(LeakArgs),
    /// Compare one metric in baseline and current result files.
    Regression(RegressionArgs),
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

#[derive(Debug, Args)]
struct ApiArgs {
    /// Total number of GET requests.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    requests: u32,
    /// Number of concurrent request workers.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..))]
    concurrency: u32,
    /// Read-only Firecrab API path.
    #[arg(long, default_value = "/api/vms", value_parser = parse_api_path)]
    path: String,
}

#[derive(Debug, Args)]
struct NetworkArgs {
    /// Host name or IP address of an iperf3 server.
    #[arg(long)]
    target: String,
    /// Test duration in seconds.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..))]
    duration: u32,
    /// Number of parallel iperf3 streams.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    parallel: u32,
    /// Measure server-to-client traffic.
    #[arg(long)]
    reverse: bool,
    /// Use UDP instead of TCP.
    #[arg(long)]
    udp: bool,
    /// UDP target bitrate accepted by iperf3.
    #[arg(long, default_value = "1G")]
    bitrate: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum StorageModeArg {
    SequentialRead,
    SequentialWrite,
    RandomRead,
    RandomWrite,
}

impl From<StorageModeArg> for StorageMode {
    fn from(mode: StorageModeArg) -> Self {
        match mode {
            StorageModeArg::SequentialRead => Self::SequentialRead,
            StorageModeArg::SequentialWrite => Self::SequentialWrite,
            StorageModeArg::RandomRead => Self::RandomRead,
            StorageModeArg::RandomWrite => Self::RandomWrite,
        }
    }
}

#[derive(Debug, Args)]
struct StorageArgs {
    /// Directory where fio creates and removes its temporary file.
    #[arg(long)]
    directory: PathBuf,
    /// Storage access pattern.
    #[arg(long, value_enum)]
    mode: StorageModeArg,
    /// fio block size.
    #[arg(long, default_value = "4k")]
    block_size: String,
    /// Temporary fio file size in MiB.
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..))]
    size_mib: u32,
    /// Number of fio jobs.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    jobs: u32,
}

#[derive(Debug, Args)]
struct SoakArgs {
    /// Test duration such as 30s, 10m, or 1h.
    #[arg(long, value_parser = parse_duration)]
    duration: Duration,
    /// Optional iteration bound for PR-sized soak runs.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    max_iterations: Option<u32>,
    #[command(flatten)]
    vm: VmArgs,
}

#[derive(Debug, Args)]
struct LeakArgs {
    /// Number of lifecycles between host resource snapshots.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..))]
    iterations: u32,
    #[command(flatten)]
    vm: VmArgs,
}

#[derive(Debug, Args)]
struct RegressionArgs {
    /// Baseline benchmark JSON file.
    #[arg(long)]
    baseline: PathBuf,
    /// Current benchmark JSON file.
    #[arg(long)]
    current: PathBuf,
    /// Latency field or command-specific metric name.
    #[arg(long, default_value = "p95_ms")]
    metric: String,
    /// Maximum permitted regression percentage.
    #[arg(long, default_value_t = 10.0)]
    threshold_percent: f64,
}

fn main() {
    let cli = Cli::parse();
    let base = resolve_api_base(cli.api.as_deref());
    let output = cli.output;
    let publish = cli.publish;
    let result = match cli.command {
        Command::Boot(args) => run_boot(&HttpVmApi::new(base.clone()), &args.vm.into(), args.count),
        Command::Create(args) => run_concurrent_creation(
            &HttpVmApi::new(base.clone()),
            &args.vm.into(),
            args.concurrency,
        ),
        Command::Density(args) => run_density(
            &HttpVmApi::new(base.clone()),
            &args.vm.into(),
            args.max_vms,
            args.step,
            Duration::from_secs(args.settle_seconds),
        ),
        Command::Lifecycle(args) => run_lifecycle(
            &HttpVmApi::new(base.clone()),
            &args.vm.into(),
            args.iterations,
        ),
        Command::Api(args) => run_api_load(&base, &args.path, args.requests, args.concurrency),
        Command::Network(args) => run_network(&NetworkConfig {
            target: args.target,
            duration_seconds: args.duration,
            parallel: args.parallel,
            reverse: args.reverse,
            udp: args.udp,
            bitrate: args.bitrate,
        }),
        Command::Storage(args) => run_storage(&StorageConfig {
            directory: args.directory,
            mode: args.mode.into(),
            block_size: args.block_size,
            size_mib: args.size_mib,
            jobs: args.jobs,
        }),
        Command::Soak(args) => run_soak(
            &HttpVmApi::new(base.clone()),
            &args.vm.into(),
            args.duration,
            args.max_iterations,
        ),
        Command::Leak(args) => run_leak_check(
            &HttpVmApi::new(base.clone()),
            &args.vm.into(),
            args.iterations,
        ),
        Command::Regression(args) => run_regression_files(
            &args.baseline,
            &args.current,
            &args.metric,
            args.threshold_percent,
        ),
    };
    let serialized = serde_json::to_string_pretty(&result).expect("result serialization");
    println!("{serialized}");
    let mut delivery_failed = false;
    if let Some(path) = output
        && let Err(error) = std::fs::write(&path, format!("{serialized}\n"))
    {
        eprintln!("failed to write {}: {error}", path.display());
        delivery_failed = true;
    }
    if publish && let Err(error) = publish_result(&base, &result) {
        eprintln!("{error}");
        delivery_failed = true;
    }
    if result.failed_count > 0 || delivery_failed {
        std::process::exit(1);
    }
}

fn parse_api_path(path: &str) -> Result<String, String> {
    if path.starts_with("/api/") {
        Ok(path.to_owned())
    } else {
        Err("path must start with /api/".to_owned())
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b's') => (&value[..value.len() - 1], 1),
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 60 * 60),
        _ => return Err("duration must end with s, m, or h".to_owned()),
    };
    let amount = number
        .parse::<u64>()
        .map_err(|_| "duration must start with a positive integer".to_owned())?;
    let seconds = amount
        .checked_mul(multiplier)
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| "duration is outside the supported range".to_owned())?;
    Ok(Duration::from_secs(seconds))
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
    fn cli_parses_all_phase_two_commands() {
        assert!(Cli::try_parse_from(["firecrab-bench", "api", "--requests", "10"]).is_ok());
        assert!(Cli::try_parse_from(["firecrab-bench", "network", "--target", "10.0.0.2"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "firecrab-bench",
                "storage",
                "--directory",
                "/tmp",
                "--mode",
                "random-read",
            ])
            .is_ok()
        );
    }

    #[test]
    fn api_command_rejects_non_api_paths() {
        assert!(Cli::try_parse_from(["firecrab-bench", "api", "--path", "/health"]).is_err());
    }

    #[test]
    fn cli_parses_all_phase_three_commands() {
        let network = Uuid::new_v4();
        let mut soak = vec![
            "firecrab-bench".to_owned(),
            "soak".to_owned(),
            "--duration".to_owned(),
            "1h".to_owned(),
        ];
        soak.extend(vm_flags(network));
        assert!(Cli::try_parse_from(soak).is_ok());

        let mut leak = vec![
            "firecrab-bench".to_owned(),
            "leak".to_owned(),
            "--iterations".to_owned(),
            "10".to_owned(),
        ];
        leak.extend(vm_flags(network));
        assert!(Cli::try_parse_from(leak).is_ok());
        assert!(
            Cli::try_parse_from([
                "firecrab-bench",
                "regression",
                "--baseline",
                "baseline.json",
                "--current",
                "current.json",
            ])
            .is_ok()
        );
    }

    #[test]
    fn cli_parses_phase_four_delivery_flags() {
        let cli = Cli::try_parse_from([
            "firecrab-bench",
            "--output",
            "result.json",
            "--publish",
            "api",
        ])
        .unwrap();
        assert_eq!(cli.output, Some(PathBuf::from("result.json")));
        assert!(cli.publish);
    }

    #[test]
    fn duration_parser_supports_phase_three_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("0s").is_err());
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
