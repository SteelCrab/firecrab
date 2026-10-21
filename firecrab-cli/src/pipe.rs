//! The service traffic path is owned by systemd, never by the API process.

#[cfg(any(target_os = "linux", test))]
mod cloudflare;
mod config;
#[cfg(any(target_os = "linux", test))]
mod engines;
#[cfg(target_os = "linux")]
mod firecrab;
mod guest;
#[cfg(target_os = "linux")]
pub(crate) mod host;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod quick;
#[cfg(any(target_os = "linux", test))]
mod status;

use std::path::PathBuf;

use clap::Subcommand;

#[cfg(target_os = "linux")]
pub(crate) const INGRESS_UNIT: &str = "firecrab-ingress.service";
#[cfg(target_os = "linux")]
pub(crate) const TUNNEL_UNIT: &str = "firecrab-tunnel.service";

#[derive(Subcommand)]
pub enum Command {
    /// Connect or remove a stable *.firecrab.dev address.
    Address {
        #[command(subcommand)]
        command: AddressCommand,
    },
    /// Manage a stable Firecrab address from a JSON intent on stdin.
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    FirecrabManage,
    /// Refresh stable address targets independently of the API.
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    FirecrabSync,
    /// Check Nginx and install the verified cloudflared connector on the host or guest.
    Install,
    /// Validate a pipe manifest and print its native Nginx configuration.
    Render {
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
        /// Print native cloudflared config instead of native Nginx config.
        #[arg(long)]
        tunnel: bool,
    },
    /// Install/enable the independent units and gracefully apply routes (root).
    Apply {
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
        #[arg(long)]
        nginx: Option<PathBuf>,
        #[arg(long)]
        cloudflared: Option<PathBuf>,
    },
    /// Inspect ingress and all connector units, without contacting firecrab-api.
    Status,
    /// Disable and stop ingress and all connector units (root).
    Disable,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    Inspect,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    Control,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    CloudflareConnect,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    CloudflareDisconnect,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    QuickConnect,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    QuickManage,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    ApplyManaged,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    ApplyBundle {
        #[arg(long)]
        nginx: Option<PathBuf>,
        #[arg(long)]
        cloudflared: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum AddressCommand {
    /// Connect VM app addresses or all saved paths of an Nginx hostname.
    Connect {
        #[arg(required_unless_present = "token_file", conflicts_with = "token_file")]
        subdomain: Option<String>,
        /// Read a dashboard token from a private file instead of process arguments.
        #[arg(long, value_name = "FILE")]
        token_file: Option<PathBuf>,
        /// VM app IP:port; repeat to distribute requests across apps.
        #[arg(
            long,
            value_name = "IP:PORT",
            conflicts_with = "nginx",
            required_unless_present = "nginx"
        )]
        app: Vec<std::net::SocketAddr>,
        #[arg(long, conflicts_with = "app", required_unless_present = "app")]
        nginx: Option<String>,
    },
    /// Remove the owned public address and its exclusively managed connector.
    Delete { subdomain: String },
}

pub fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Address { command } => {
            use firecrab_helper_protocol::firecrab_tunnel::{Request, Target};
            let request = match command {
                AddressCommand::Connect {
                    subdomain,
                    token_file,
                    app,
                    nginx,
                } => {
                    let target = match (app.is_empty(), nginx) {
                        (false, None) => Target::Apps { upstreams: app },
                        (true, Some(host)) => Target::Nginx { host },
                        _ => return Err("Select either --app or --nginx".into()),
                    };
                    match (token_file, subdomain) {
                        (Some(path), None) => {
                            use std::io::Read;
                            let mut token = String::new();
                            std::fs::File::open(path)?
                                .take(4097)
                                .read_to_string(&mut token)?;
                            Request::Connect {
                                token: firecrab_helper_protocol::firecrab_tunnel::ConnectionToken(
                                    token.trim().to_owned(),
                                ),
                                target,
                            }
                        }
                        (None, Some(subdomain)) => Request::Save { subdomain, target },
                        _ => {
                            return Err(
                                "Provide --token-file to connect, or a saved address to edit"
                                    .into(),
                            );
                        }
                    }
                }
                AddressCommand::Delete { subdomain } => Request::Delete { subdomain },
            };
            request.validate()?;
            #[cfg(target_os = "linux")]
            {
                firecrab::manage_request(request)
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                use std::io::{Seek, Write};
                let mut input = tempfile::tempfile()?;
                input.write_all(&serde_json::to_vec(&request)?)?;
                input.rewind()?;
                guest::run(vec!["firecrab-manage".into()], Some(input))
            }
        }
        #[cfg(target_os = "linux")]
        Command::FirecrabManage => firecrab::manage(),
        #[cfg(target_os = "linux")]
        Command::FirecrabSync => firecrab::sync(),
        #[cfg(target_os = "linux")]
        Command::CloudflareConnect => cloudflare::connect(),
        #[cfg(target_os = "linux")]
        Command::CloudflareDisconnect => cloudflare::disconnect(),
        #[cfg(target_os = "linux")]
        Command::QuickConnect => quick::connect(),
        #[cfg(target_os = "linux")]
        Command::QuickManage => quick::manage(),
        #[cfg(target_os = "linux")]
        Command::Inspect => host::inspect(),
        #[cfg(target_os = "linux")]
        Command::Control => host::control(),
        #[cfg(target_os = "linux")]
        Command::ApplyManaged => host::apply_managed(),
        #[cfg(target_os = "linux")]
        Command::Install => engines::install(),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::Install => guest::run(vec!["install".into()], None),
        Command::Render { config, tunnel } => {
            let config = config::Config::read(&config)?;
            if tunnel {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &config.tunnel_config().ok_or("the manifest has no tunnel")?
                    )?
                );
            } else {
                print!("{}", config.nginx());
            }
            Ok(())
        }
        #[cfg(target_os = "linux")]
        Command::Apply {
            config,
            nginx,
            cloudflared,
        } => host::apply(&config, nginx.as_deref(), cloudflared.as_deref()),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::Apply {
            config,
            nginx,
            cloudflared,
        } => {
            let mut args = vec!["apply-bundle".into()];
            for (flag, path) in [("--nginx", nginx), ("--cloudflared", cloudflared)] {
                if let Some(path) = path {
                    args.extend([
                        flag.into(),
                        path.to_str()
                            .ok_or("guest executable paths must be UTF-8")?
                            .to_owned(),
                    ]);
                }
            }
            guest::run(args, Some(guest::input(&config)?))
        }
        #[cfg(target_os = "linux")]
        Command::Status => host::status(),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::Status => guest::run(vec!["status".into()], None),
        #[cfg(target_os = "linux")]
        Command::Disable => host::disable(),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        Command::Disable => guest::run(vec!["disable".into()], None),
        #[cfg(target_os = "linux")]
        Command::ApplyBundle { nginx, cloudflared } => {
            host::apply_bundle(nginx.as_deref(), cloudflared.as_deref())
        }
    }
}
