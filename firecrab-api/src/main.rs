mod artifacts;
mod bootstrap;
mod console;
mod error;
mod extract;
mod firecracker;
mod guest_agent;
mod guest_ssh;
mod handlers;
mod image_install;
mod ipam;
mod kernel_manager;
mod m2image_manifest;
mod microboot;
mod model;
mod network;
mod network_policy;
mod oci;
mod package;
mod persistence;
mod process_metrics;
mod rootfs;
mod server;
mod shells;
mod state;
mod storage;
mod templates;

use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::process::ExitCode;

use persistence::PersistenceError;
use server::{ConfigError, HttpConfig, build_router};
use state::AppState;
use templates::{TemplateError, TemplateRegistry};
use thiserror::Error;

#[derive(Debug, Error)]
enum StartupError {
    #[error("failed to load HTTP configuration")]
    Config(#[source] ConfigError),
    #[error("failed to initialize template registry")]
    Template(#[source] TemplateError),
    #[error("failed to load persisted VM state")]
    Persistence(#[source] PersistenceError),
    #[error("failed to bind API listener at {address}")]
    Bind {
        address: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("failed to inspect API listener address")]
    LocalAddress(#[source] io::Error),
    #[error("API server terminated with an error")]
    Serve(#[source] io::Error),
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[ERROR] {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("[ERROR] caused by: {cause}");
                source = cause.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "firecrab_api=info".into()),
        )
        .init();
}

async fn run() -> Result<(), StartupError> {
    init_tracing();
    let config = HttpConfig::load().map_err(StartupError::Config)?;
    let templates = TemplateRegistry::load_default().map_err(StartupError::Template)?;
    let state = AppState::new(templates)
        .await
        .map_err(StartupError::Persistence)?;
    // Bridges, nftables rules and dnsmasq's config are all host state a
    // reboot wipes, so they are re-applied here rather than assumed. Doing it
    // at startup (not only on VM start) is what brings back a MicroNetwork
    // that has no VMs in it yet — nothing else would ever touch it.
    //
    // Best-effort: if the net-helper isn't up yet, this just means the host
    // side lags until the next per-VM start, which re-applies the same thing
    // (see setup_vm_network) — not worth failing API startup over.
    if let Err(error) = handlers::micro_networks::ensure_all_networks(&state).await {
        tracing::warn!(error, "initial network resync failed");
    }
    // Fetch the shared bootstrap builder source now, in the background, so
    // the request that needs it doesn't have to — see spawn_warmup.
    microboot::spawn_warmup(state.clone());
    let app = build_router(state, &config);

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .map_err(|source| StartupError::Bind {
            address: config.bind_addr,
            source,
        })?;

    let local_address = listener.local_addr().map_err(StartupError::LocalAddress)?;
    tracing::info!(address = %local_address, "listening on http://{local_address}");
    axum::serve(listener, app)
        .await
        .map_err(StartupError::Serve)?;
    Ok(())
}
