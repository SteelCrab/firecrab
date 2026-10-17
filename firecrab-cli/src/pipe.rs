//! The service traffic path is owned by systemd, never by the API process.

#[cfg(any(target_os = "linux", test))]
mod cloudflare;
mod config;
#[cfg(any(target_os = "linux", test))]
mod engines;
mod guest;
#[cfg(target_os = "linux")]
pub(crate) mod host;

use std::path::PathBuf;

use clap::Subcommand;

#[cfg(target_os = "linux")]
pub(crate) const INGRESS_UNIT: &str = "firecrab-ingress.service";
#[cfg(target_os = "linux")]
pub(crate) const TUNNEL_UNIT: &str = "firecrab-tunnel.service";

#[derive(Subcommand)]
pub enum Command {
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
    /// Inspect both data-plane units, without contacting firecrab-api.
    Status,
    /// Disable and stop the tunnel and ingress units (root).
    Disable,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    Inspect,
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    CloudflareConnect,
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

pub fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        #[cfg(target_os = "linux")]
        Command::CloudflareConnect => cloudflare::connect(),
        #[cfg(target_os = "linux")]
        Command::Inspect => host::inspect(),
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
