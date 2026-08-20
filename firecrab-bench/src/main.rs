use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use firecrab_api_types::{VmResponse, VmState};
use firecrab_bench::BenchmarkResult;
use reqwest::blocking::{Client, Response};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

const DEFAULT_API_BASE: &str = "http://127.0.0.1:5523";
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);

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
    /// Measure create-request through running-state boot latency.
    Boot(BootArgs),
}

#[derive(Debug, clap::Args)]
struct BootArgs {
    /// Number of sequential VM boots to measure.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    count: u32,
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

/// API request body for an unmodified benchmark VM.
///
/// `CreateVmRequest` is intentionally deserialize-only because the API owns
/// its wire validation. The benchmark client needs the same JSON contract in
/// the opposite direction, so it keeps this narrow private representation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateVmBody {
    name: String,
    template: String,
    ram: u32,
    cpu: u8,
    disk_gb: u16,
    egress_policy: &'static str,
    micro_network_id: Uuid,
}

#[derive(Debug, Error)]
enum BenchmarkError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("VM {0} did not reach running state within the boot timeout")]
    Timeout(Uuid),
    #[error("VM {0} entered error state during boot")]
    FailedBoot(Uuid),
}

fn main() {
    let cli = Cli::parse();
    let base = resolve_api_base(cli.api.as_deref());
    let result = match cli.command {
        Command::Boot(args) => run_boot(&base, args),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("result serialization")
    );
    if result.failed_count > 0 {
        std::process::exit(1);
    }
}

fn run_boot(base: &str, args: BootArgs) -> BenchmarkResult {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client construction");
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for sequence in 1..=args.count {
        match boot_once(&client, base, &args, sequence) {
            Ok(elapsed) => samples.push(elapsed),
            Err(error) => failures.push(format!("iteration {sequence}: {error}")),
        }
    }
    BenchmarkResult::new("vm_boot", args.count, &samples, failures)
}

fn boot_once(
    client: &Client,
    base: &str,
    args: &BootArgs,
    sequence: u32,
) -> Result<Duration, BenchmarkError> {
    let started = Instant::now();
    let request = CreateVmBody {
        name: format!("bench-boot-{sequence}-{}", Uuid::new_v4().simple()),
        template: args.template.clone(),
        ram: args.ram,
        cpu: args.cpu,
        disk_gb: args.disk_gb,
        egress_policy: "internet",
        micro_network_id: args.micro_network_id,
    };
    let vm = checked(
        client
            .post(format!("{base}/api/vms"))
            .json(&request)
            .send()?,
    )?
    .json::<VmResponse>()?;
    let result = (|| {
        checked(
            client
                .post(format!("{base}/api/vms/{}/start", vm.id))
                .send()?,
        )?;
        wait_for_running(client, base, vm.id)?;
        Ok(started.elapsed())
    })();
    cleanup_vm(client, base, vm.id);
    result
}

fn wait_for_running(client: &Client, base: &str, id: Uuid) -> Result<(), BenchmarkError> {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    loop {
        let vm =
            checked(client.get(format!("{base}/api/vms/{id}")).send()?)?.json::<VmResponse>()?;
        match vm.state {
            VmState::Running => return Ok(()),
            VmState::Error => return Err(BenchmarkError::FailedBoot(id)),
            _ if Instant::now() >= deadline => return Err(BenchmarkError::Timeout(id)),
            _ => thread::sleep(POLL_INTERVAL),
        }
    }
}

fn cleanup_vm(client: &Client, base: &str, id: Uuid) {
    let state = client
        .get(format!("{base}/api/vms/{id}"))
        .send()
        .ok()
        .and_then(|response| response.json::<VmResponse>().ok())
        .map(|vm| vm.state);
    if matches!(state, Some(VmState::Running)) {
        let _ = client.post(format!("{base}/api/vms/{id}/stop")).send();
    }
    let _ = client.delete(format!("{base}/api/vms/{id}")).send();
}

fn checked(response: Response) -> Result<Response, BenchmarkError> {
    if response.status().is_success() {
        Ok(response)
    } else {
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        Err(BenchmarkError::Http { status, body })
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

    #[test]
    fn cli_parses_boot_command() {
        let network = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "firecrab-bench",
            "boot",
            "--count",
            "5",
            "--template",
            "ubuntu",
            "--micro-network-id",
            &network.to_string(),
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Boot(BootArgs { count: 5, .. })
        ));
    }

    #[test]
    fn api_base_strips_a_trailing_slash() {
        assert_eq!(
            resolve_api_base(Some("http://example.test/")),
            "http://example.test"
        );
    }

    #[test]
    fn boot_benchmark_preserves_an_unreachable_api_failure() {
        let result = run_boot(
            "http://127.0.0.1:1",
            BootArgs {
                count: 1,
                template: "ubuntu".to_owned(),
                micro_network_id: Uuid::new_v4(),
                ram: 512,
                cpu: 1,
                disk_gb: 8,
            },
        );
        assert_eq!(result.successful_count, 0);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.failure_rate, 100.0);
    }
}
