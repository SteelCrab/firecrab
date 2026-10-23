//! Offline validation plus host-local ownership and read-only plan commands.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use firecrab_stack::{
    Binding, DeleteStackRequest, Plan, RegisterStackRequest, ResourceKind, StackRecord,
    logical_name,
};
use firecrab_stack::{Diagnostic, MAX_MANIFEST_BYTES, ResourceCounts};
use serde::Serialize;
use uuid::Uuid;

use crate::api_client::{ApiClient, ApiError};

/// Stack validation, ownership metadata and read-only planning. No apply is implemented.
#[derive(Debug, Subcommand)]
pub enum Command {
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

/// Use the selected API for ownership metadata and read-only plans.
pub fn run_remote(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
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
                println!("KIND\tKEY\tACTION\tFIELDS / REASON");
                for change in &plan.changes {
                    println!(
                        "{}\t{}\t{:?}\t{}",
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
