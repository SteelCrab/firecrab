//! Local stack validation; intentionally bypasses host profiles and API clients.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use firecrab_stack::{Diagnostic, MAX_MANIFEST_BYTES, ResourceCounts};
use serde::Serialize;

/// Offline commands available before stack ownership and apply are implemented.
#[derive(Debug, Subcommand)]
pub enum Command {
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
pub fn run(command: Command) -> i32 {
    let Command::Validate { file, json } = command;
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
