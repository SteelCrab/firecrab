use std::fmt::Write;

use clap::Subcommand;
use firecrab_api_types::{
    ImageInstallResponse, ImageInstallStatus, ImageResponse, OciImportRequest, OciInspectResponse,
};

use crate::api_client::{ApiClient, ApiError};

/// Template image operations exposed by `firecrab image`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List known template images and their local install state.
    List {
        /// Emit the API response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resolve an OCI reference without downloading its layers.
    Inspect {
        /// OCI reference accepted by `docker pull`, such as `nginx:1.27`.
        reference: String,
        /// Emit the inspection response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Start a background OCI import.
    Import {
        /// OCI reference accepted by `docker pull`, such as `nginx:1.27`.
        reference: String,
        /// Emit the initial import snapshot as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the latest background OCI import snapshot.
    ImportStatus {
        /// Derived image alias returned by `image inspect` or `image import`.
        alias: String,
        /// Emit the import snapshot as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Errors produced while executing an image command.
#[derive(Debug)]
pub enum Error {
    /// HTTP, transport, or response decoding failure.
    Api(ApiError),
    /// A background OCI import reached the failed state.
    ImportFailed { alias: String, detail: String },
}

impl From<ApiError> for Error {
    fn from(value: ApiError) -> Self {
        Self::Api(value)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api(error) => error.fmt(f),
            Self::ImportFailed { alias, detail } => {
                write!(f, "OCI import {alias} failed: {detail}")
            }
        }
    }
}

/// Executes one image command through the host API.
pub fn run(client: &ApiClient, command: Command) -> Result<(), Error> {
    match command {
        Command::List { json } => {
            let images: Vec<ImageResponse> = client.get("/api/images")?;
            if json {
                print_json(&images);
            } else {
                print!("{}", format_list(&images));
            }
        }
        Command::Inspect { reference, json } => {
            let inspect: OciInspectResponse =
                client.get_query("/api/oci/inspect", &[("reference", reference.as_str())])?;
            if json {
                print_json(&inspect);
            } else {
                print!("{}", format_inspect(&inspect));
            }
        }
        Command::Import { reference, json } => {
            let snapshot: ImageInstallResponse =
                client.post("/api/oci/import", &OciImportRequest { reference })?;
            print_import_snapshot(&snapshot, json);
            ensure_import_succeeded_or_active(&snapshot)?;
        }
        Command::ImportStatus { alias, json } => {
            let snapshot: ImageInstallResponse = client.get(&format!("/api/oci/import/{alias}"))?;
            print_import_snapshot(&snapshot, json);
            ensure_import_succeeded_or_active(&snapshot)?;
        }
    }
    Ok(())
}

fn print_json(value: &(impl serde::Serialize + ?Sized)) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("API response serializes")
    );
}

fn format_list(images: &[ImageResponse]) -> String {
    let mut output = String::from("ALIAS\tVERSION\tINSTALLED\tMIN_DISK_GIB\tPACKAGE\n");
    for image in images {
        let package = if image.package_staged {
            "staged"
        } else if image.package_url.is_some() {
            "available"
        } else {
            "-"
        };
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            image.alias,
            image.version,
            if image.installed { "yes" } else { "no" },
            image.min_disk_gb,
            package
        ));
    }
    output
}

fn format_inspect(inspect: &OciInspectResponse) -> String {
    let mut output = String::new();
    writeln!(output, "OCI IMAGE {}", inspect.alias).unwrap();
    writeln!(output, "  registry:        {}", inspect.registry).unwrap();
    writeln!(output, "  repository:      {}", inspect.repository).unwrap();
    writeln!(output, "  version:         {}", inspect.version).unwrap();
    writeln!(output, "  digest:          {}", inspect.digest).unwrap();
    writeln!(output, "  architecture:    {}", inspect.architecture).unwrap();
    writeln!(
        output,
        "  immutable:       {}",
        if inspect.immutable { "yes" } else { "no" }
    )
    .unwrap();
    writeln!(
        output,
        "  single platform: {}",
        if inspect.single_platform { "yes" } else { "no" }
    )
    .unwrap();
    output
}

fn print_import_snapshot(snapshot: &ImageInstallResponse, json: bool) {
    if json {
        print_json(snapshot);
    } else {
        print!("{}", format_import_snapshot(snapshot));
    }
}

fn format_import_snapshot(snapshot: &ImageInstallResponse) -> String {
    let mut output = format!(
        "OCI import {}: {}\n",
        snapshot.alias,
        install_status(snapshot.status)
    );
    if !snapshot.log.trim().is_empty() {
        output.push_str("  log:\n");
        for line in snapshot.log.lines() {
            writeln!(output, "    {line}").unwrap();
        }
    }
    if snapshot.status == ImageInstallStatus::Running {
        writeln!(
            output,
            "  poll: firecrab image import-status {}",
            snapshot.alias
        )
        .unwrap();
    }
    output
}

fn install_status(status: ImageInstallStatus) -> &'static str {
    match status {
        ImageInstallStatus::Idle => "idle",
        ImageInstallStatus::Running => "running",
        ImageInstallStatus::Succeeded => "succeeded",
        ImageInstallStatus::Failed => "failed",
    }
}

fn ensure_import_succeeded_or_active(snapshot: &ImageInstallResponse) -> Result<(), Error> {
    if snapshot.status != ImageInstallStatus::Failed {
        return Ok(());
    }
    let detail = snapshot
        .log
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no failure detail returned")
        .trim()
        .to_owned();
    Err(Error::ImportFailed {
        alias: snapshot.alias.clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    fn image(installed: bool, staged: bool) -> ImageResponse {
        ImageResponse {
            alias: "alpine-3.24.1".to_owned(),
            version: "3.24.1".to_owned(),
            kernel_version: None,
            kernel_image: "vmlinuz".to_owned(),
            kernel_sha256: String::new(),
            rootfs_sha256: String::new(),
            initrd_sha256: None,
            min_disk_gb: 2,
            rootfs_size_bytes: 0,
            installed,
            package_url: Some("https://example.test/alpine.tar.zst".to_owned()),
            package_staged: staged,
            package_origin: None,
            description: "Alpine".to_owned(),
            has_guest_service: false,
        }
    }

    #[test]
    fn empty_list_still_has_a_header() {
        assert_eq!(
            format_list(&[]),
            "ALIAS\tVERSION\tINSTALLED\tMIN_DISK_GIB\tPACKAGE\n"
        );
    }

    #[test]
    fn list_formats_install_and_package_state() {
        let output = format_list(&[image(true, false), image(false, true)]);
        assert!(output.contains("alpine-3.24.1\t3.24.1\tyes\t2\tavailable"));
        assert!(output.contains("alpine-3.24.1\t3.24.1\tno\t2\tstaged"));
    }

    #[test]
    fn oci_commands_parse_reference_alias_and_json() {
        let inspect = TestCli::try_parse_from(["test", "inspect", "nginx:1.27", "--json"]).unwrap();
        assert!(matches!(
            inspect.command,
            Command::Inspect {
                reference,
                json: true
            } if reference == "nginx:1.27"
        ));

        let import = TestCli::try_parse_from(["test", "import", "nginx:1.27"]).unwrap();
        assert!(matches!(
            import.command,
            Command::Import {
                reference,
                json: false
            } if reference == "nginx:1.27"
        ));

        let status = TestCli::try_parse_from(["test", "import-status", "nginx-1.27"]).unwrap();
        assert!(matches!(
            status.command,
            Command::ImportStatus {
                alias,
                json: false
            } if alias == "nginx-1.27"
        ));
    }

    #[test]
    fn inspect_human_includes_resolved_manifest_and_alias() {
        let output = format_inspect(&OciInspectResponse {
            registry: "docker.io".to_owned(),
            repository: "library/nginx".to_owned(),
            version: "1.27".to_owned(),
            immutable: false,
            digest: "sha256:abc".to_owned(),
            architecture: "amd64".to_owned(),
            single_platform: false,
            alias: "nginx-1.27".to_owned(),
        });
        assert!(output.contains("OCI IMAGE nginx-1.27"));
        assert!(output.contains("repository:      library/nginx"));
        assert!(output.contains("digest:          sha256:abc"));
        assert!(output.contains("architecture:    amd64"));
    }

    #[test]
    fn running_import_names_the_poll_command() {
        let output = format_import_snapshot(&import_snapshot(ImageInstallStatus::Running));
        assert!(output.contains("OCI import nginx-1.27: running"));
        assert!(output.contains("firecrab image import-status nginx-1.27"));
    }

    #[test]
    fn failed_import_uses_the_last_non_empty_log_line() {
        let error = ensure_import_succeeded_or_active(&ImageInstallResponse {
            log: "pulling layers\n\nregistry rejected blob\n".to_owned(),
            ..import_snapshot(ImageInstallStatus::Failed)
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "OCI import nginx-1.27 failed: registry rejected blob"
        );
    }

    fn import_snapshot(status: ImageInstallStatus) -> ImageInstallResponse {
        ImageInstallResponse {
            alias: "nginx-1.27".to_owned(),
            status,
            log: "import started".to_owned(),
            started_at_ms: Some(1),
            ended_at_ms: None,
            downloaded_bytes: None,
            total_bytes: None,
        }
    }
}
