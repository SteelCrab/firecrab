//! Offline validation, host-local ownership, planning and explicit native apply.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use firecrab_stack::{
    ApplyRequest, ApplyState, ApplyStatus, Binding, DeleteStackRequest, Plan, RegisterStackRequest,
    ResourceKind, StackRecord, logical_name,
};
use firecrab_stack::{Diagnostic, MAX_MANIFEST_BYTES, ResourceCounts};
use serde::Serialize;
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};
mod source;

/// Stack validation, ownership metadata, planning and explicit native execution.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Configure a Git source and detect drift without applying resources.
    Source {
        #[command(subcommand)]
        command: source::Command,
    },
    /// Apply a fresh or explicitly reviewed plan with durable retry identity.
    Apply(ApplyArgs),
    /// Read durable, redacted execution status after a disconnect or API restart.
    ApplyStatus {
        /// Registered stack name.
        name: String,
        /// Exact operation UUID printed by stack apply.
        #[arg(long)]
        id: Uuid,
    },
    /// Register host-local stack ownership metadata without applying resources.
    Register {
        /// Stable name matching the manifest's name.
        #[arg(long)]
        name: String,
    },
    /// List registered stacks and their UUID ownership mappings.
    List,
    /// Read a stack's ownership mappings and revision.
    Status { name: String },
    /// Delete empty registration metadata; never delete bound resources.
    Delete {
        /// Registered stack name on the selected host.
        name: String,
        /// Exact registration UUID from stack status.
        #[arg(long)]
        id: Uuid,
    },
    /// Explicitly connect an existing resource UUID to a logical manifest key.
    Bind(BindingArgs),
    /// Release an exact ownership mapping; keep the resource and its disks.
    Unbind(BindingArgs),
    /// Compare declarations to owned host configuration. Does not apply changes.
    Plan {
        /// YAML declaration to compare with the selected host.
        #[arg(long)]
        file: PathBuf,
        /// Print the redacted plan as JSON, including blocked entries.
        #[arg(long)]
        json: bool,
    },
    /// Check one YAML stack's schema, limits and local references. Does not deploy.
    Validate {
        /// UTF-8 YAML manifest, limited to 1 MiB.
        #[arg(long)]
        file: PathBuf,
        /// Print a JSON validation report, including failures.
        #[arg(long)]
        json: bool,
    },
}

/// Native apply controls. Creating resources never requires an interruption flag.
#[derive(Debug, clap::Args)]
pub struct ApplyArgs {
    /// Desired YAML declaration.
    #[arg(long)]
    file: PathBuf,
    /// Retry the exact same declaration and stop permission with this UUID.
    #[arg(long)]
    operation_id: Option<Uuid>,
    /// JSON output from stack plan; fail if its configuration/ownership is stale.
    #[arg(long)]
    expected_plan: Option<PathBuf>,
    /// Permit stopping owned running VMs and replacing owned service replicas.
    #[arg(long)]
    allow_stop: bool,
    /// Return after durable acceptance without waiting for completion.
    #[arg(long)]
    no_wait: bool,
    /// Maximum polling time in seconds; timeout does not cancel accepted work.
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..=86400))]
    timeout: u64,
    /// Print the final or accepted redacted status as JSON.
    #[arg(long)]
    json: bool,
}

/// Exact ownership identity shared by bind and unbind.
#[derive(Debug, clap::Args)]
pub struct BindingArgs {
    /// Registered stack name on the selected host.
    #[arg(long)]
    stack: String,
    /// Resource kind: network, vm, or service.
    #[arg(long)]
    kind: ResourceKind,
    /// Logical map key in the manifest.
    #[arg(long)]
    key: String,
    /// Exact existing resource UUID, also required when unbinding.
    #[arg(long)]
    id: Uuid,
}

/// Remote metadata and plan failures. No manifest values are included.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("{0}")]
    Validation(String),
    #[error("plan is blocked; resolve explicit ownership and network dependencies")]
    Blocked,
    #[error("apply {id} ended in {state}; inspect stack apply-status before explicit recovery")]
    ApplyFailed { id: Uuid, state: String },
    #[error(
        "apply {0} is still running; use stack apply-status or retry with the same operation ID"
    )]
    ApplyTimeout(Uuid),
}

#[derive(Serialize)]
struct Report {
    valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<ResourceCounts>,
    errors: Vec<Diagnostic>,
}

/// Exit zero only for a valid manifest; do not echo submitted configuration values.
pub fn validate_file(file: PathBuf, json: bool) -> i32 {
    let report = match read_manifest(&file).and_then(|source| firecrab_stack::parse(&source)) {
        Ok(stack) => Report {
            valid: true,
            resources: Some(stack.counts()),
            stack: Some(stack.name),
            errors: Vec::new(),
        },
        Err(errors) => Report {
            valid: false,
            stack: None,
            resources: None,
            errors,
        },
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("validation reports are JSON values")
        );
    } else if let (Some(name), Some(counts)) = (&report.stack, &report.resources) {
        println!(
            "Stack {name} is valid: {} networks, {} VMs, {} services.",
            counts.micro_networks, counts.micro_vms, counts.micro_services
        );
        println!("Offline validation only; host state and deployment readiness were not checked.");
    } else {
        for error in &report.errors {
            match (error.line, error.column) {
                (Some(line), Some(column)) => eprintln!(
                    "{} at line {line}, column {column}: {}",
                    error.path, error.message
                ),
                _ => eprintln!("{}: {}", error.path, error.message),
            }
        }
    }
    i32::from(!report.valid)
}

/// Use the selected API for ownership metadata, plans and durable apply operations.
pub fn run_remote(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::Source { command } => source::run(client, command)?,
        Command::Apply(args) => apply_file(client, args)?,
        Command::ApplyStatus { name, id } => {
            name_valid(&name)?;
            if id.is_nil() {
                return Err(Error::Validation(
                    "a non-nil operation UUID is required".into(),
                ));
            }
            let status: ApplyStatus = client.get(&format!("/api/stacks/{name}/applies/{id}"))?;
            print_json(&status);
        }
        Command::Register { name } => {
            name_valid(&name)?;
            let stack: StackRecord = client.post("/api/stacks", &RegisterStackRequest { name })?;
            print_json(&stack);
        }
        Command::List => {
            let stacks: Vec<StackRecord> = client.get("/api/stacks")?;
            print_json(&stacks);
        }
        Command::Status { name } => {
            name_valid(&name)?;
            let stack: StackRecord = client.get(&format!("/api/stacks/{name}"))?;
            print_json(&stack);
        }
        Command::Delete { name, id } => {
            name_valid(&name)?;
            if id.is_nil() {
                return Err(Error::Validation(
                    "a non-nil stack UUID is required".to_owned(),
                ));
            }
            client.delete_query(&format!("/api/stacks/{name}"), &DeleteStackRequest { id })?;
        }
        Command::Bind(args) => write_binding(client, args, false)?,
        Command::Unbind(args) => write_binding(client, args, true)?,
        Command::Plan { file, json } => {
            let source = read_manifest(&file).map_err(diagnostics)?;
            let stack = firecrab_stack::parse(&source).map_err(diagnostics)?;
            let plan: Plan = client.post(&format!("/api/stacks/{}/plan", stack.name), &stack)?;
            if json {
                print_json(&plan);
            } else {
                println!(
                    "Stack {} (ownership revision {}): read-only plan",
                    plan.name, plan.revision
                );
                println!("Plan fingerprint: {}", plan.fingerprint);
                println!("KIND\tKEY\tACTION\tFIELDS / REASON");
                for change in &plan.changes {
                    println!(
                        "{}\t{}\t{}\t{}",
                        change.kind,
                        change.key,
                        change.action,
                        change
                            .reason
                            .as_deref()
                            .map(str::to_owned)
                            .unwrap_or_else(|| change.fields.join(", "))
                    );
                }
            }
            if plan.blocked {
                return Err(Error::Blocked);
            }
        }
        Command::Validate { .. } => {
            return Err(Error::Validation(
                "validation must run through the offline command".to_owned(),
            ));
        }
    }
    Ok(())
}

fn apply_file(client: &ApiClient, args: ApplyArgs) -> Result<(), Error> {
    let source = read_manifest(&args.file).map_err(diagnostics)?;
    let stack = firecrab_stack::parse(&source).map_err(diagnostics)?;
    let operation_id = args.operation_id.unwrap_or_else(Uuid::new_v4);
    if operation_id.is_nil() {
        return Err(Error::Validation(
            "a non-nil operation UUID is required".into(),
        ));
    }
    let plan: Plan = if let Some(file) = args.expected_plan {
        let source = read_manifest(&file).map_err(diagnostics)?;
        serde_json::from_str(&source).map_err(|_| {
            Error::Validation("expected-plan must contain the JSON output of stack plan".into())
        })?
    } else {
        client.post(&format!("/api/stacks/{}/plan", stack.name), &stack)?
    };
    if plan.name != stack.name || plan.fingerprint.len() != 64 {
        return Err(Error::Validation(
            "expected plan must match the manifest name and include its fingerprint".into(),
        ));
    }
    let name = stack.name.clone();
    eprintln!("Apply operation: {operation_id}");
    let request = ApplyRequest {
        stack,
        expected_fingerprint: plan.fingerprint,
        operation_id,
        allow_stop: args.allow_stop,
    };
    let mut status: ApplyStatus = client.post(&format!("/api/stacks/{name}/apply"), &request)?;
    let started = std::time::Instant::now();
    let mut progress = None;
    while !args.no_wait && !status.state.is_terminal() {
        let completed = status
            .changes
            .iter()
            .filter(|s| s.state == firecrab_stack::StepState::Succeeded)
            .count();
        let current = (status.state, completed);
        if progress != Some(current) {
            eprintln!(
                "Apply {}: {}, {completed}/{} steps complete",
                operation_id,
                status.state.as_str(),
                status.changes.len()
            );
            progress = Some(current);
        }
        if started.elapsed() >= std::time::Duration::from_secs(args.timeout) {
            print_apply_status(&status, args.json);
            return Err(Error::ApplyTimeout(operation_id));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        status = client.get(&format!("/api/stacks/{name}/applies/{operation_id}"))?;
    }
    print_apply_status(&status, args.json);
    if status.state.is_terminal() && status.state != ApplyState::Succeeded {
        return Err(Error::ApplyFailed {
            id: operation_id,
            state: status.state.as_str().into(),
        });
    }
    Ok(())
}

fn print_apply_status(status: &ApplyStatus, json: bool) {
    if json {
        print_json(status);
        return;
    }
    println!("Apply {}: {}", status.operation_id, status.state.as_str());
    println!("KIND\tKEY\tACTION\tRESOURCE UUID\tSTATE");
    for step in &status.changes {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            step.change.kind,
            step.change.key,
            step.change.action,
            step.change
                .resource_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            step.state.as_str()
        );
        if step.state == firecrab_stack::StepState::Succeeded
            && !status.lifecycle.delete_vm_disks
            && let Some(disk) = &step.vm_disk
        {
            println!(
                "Retained VM disk artifacts: storage={}, vm={}, generation={}",
                disk.storage_root,
                step.change
                    .resource_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                disk.generation
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unprepared".into())
            );
        }
    }
    if let Some(error) = &status.error {
        eprintln!(
            "{}: {} ({})",
            error.code,
            error.message,
            error.fields.join(", ")
        );
    }
}

fn write_binding(client: &ApiClient, args: BindingArgs, remove: bool) -> Result<(), Error> {
    name_valid(&args.stack)?;
    name_valid(&args.key)?;
    if args.id.is_nil() {
        return Err(Error::Validation(
            "resource UUID must not be nil".to_owned(),
        ));
    }
    let suffix = if remove { "unbind" } else { "bindings" };
    let binding = Binding {
        kind: args.kind,
        key: args.key,
        resource_id: args.id,
    };
    let stack: StackRecord =
        client.post(&format!("/api/stacks/{}/{suffix}", args.stack), &binding)?;
    print_json(&stack);
    Ok(())
}

fn name_valid(name: &str) -> Result<(), Error> {
    if logical_name(name) {
        return Ok(());
    }
    Err(Error::Validation(
        "stack names and logical keys must be lowercase DNS labels of 1-63 bytes".to_owned(),
    ))
}

fn diagnostics(errors: Vec<Diagnostic>) -> Error {
    Error::Validation(
        errors
            .into_iter()
            .map(|e| format!("{}: {}", e.path, e.message))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn print_json(value: &impl Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("stack metadata and plans are JSON values")
    );
}

fn read_manifest(path: &Path) -> Result<String, Vec<Diagnostic>> {
    let failure = |message| {
        vec![Diagnostic {
            path: "$".to_owned(),
            message,
            line: None,
            column: None,
        }]
    };
    let file = std::fs::File::open(path).map_err(|_| failure("could not open manifest file"))?;
    let mut source = Vec::new();
    file.take((MAX_MANIFEST_BYTES + 1) as u64)
        .read_to_end(&mut source)
        .map_err(|_| failure("could not read manifest file"))?;
    if source.len() > MAX_MANIFEST_BYTES {
        return Err(failure("manifest exceeds 1 MiB"));
    }
    String::from_utf8(source).map_err(|_| failure("manifest must be UTF-8"))
}
