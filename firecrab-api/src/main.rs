mod artifacts;
mod block_storage;
mod bootstrap;
mod console;
mod console_replay;
mod console_resize;
mod console_session;
mod error;
mod extract;
mod firecracker;
mod guest_agent;
mod guest_console;
mod guest_ssh;
mod handlers;
mod host_platform;
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
mod pipe_ai;
mod pipe_auth;
mod pipe_databases;
mod pipe_storage;
mod pool;
mod process_metrics;
mod reconcile;
mod resource_limits;
mod rootfs;
mod scheduler;
mod secrets;
mod server;
mod service_runtime;
mod shells;
mod stack_source;
mod state;
mod storage;
mod templates;
mod tls;
mod vm_execution;
mod vm_shim;

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
    #[error("failed to recover data volumes: {0}")]
    VolumeRecovery(String),
    #[error("failed to load HTTP configuration")]
    Config(#[source] ConfigError),
    #[error("failed to configure API TLS")]
    Tls(#[source] firecrab_pki::Error),
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

fn main() -> ExitCode {
    // `firecrab-api vm-shim …` runs one VM's shim instead of the API; see
    // `vm_shim`. Checked before anything else so the shim never opens the
    // database or binds the API port.
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new(vm_shim::SUBCOMMAND)) {
        return vm_shim::run(args.collect());
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[ERROR] failed to start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(serve_api())
}

async fn serve_api() -> ExitCode {
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
    let transport = tls::configuration(
        config.bind_addr,
        firecrab_pki::TlsPaths::from_env().map_err(StartupError::Tls)?,
    )
    .map_err(StartupError::Tls)?;
    let templates = TemplateRegistry::load_default().map_err(StartupError::Template)?;
    let mut state = AppState::new(templates)
        .await
        .map_err(StartupError::Persistence)?;
    // Reject a second owner before recovering any host or scheduled work.
    let scheduler_ownership = state
        .store
        .scheduler_lock()
        .map_err(StartupError::Persistence)?;
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.interrupt_stack_checks(stack_source::now()))
        .await
        .expect("Git source recovery task panicked")
        .map_err(StartupError::Persistence)?;
    // Bridges, nftables rules and dnsmasq's config are all host state a
    // reboot wipes, so they are re-applied here rather than assumed. Doing it
    // at startup (not only on VM start) is what brings back a MicroNetwork
    // that has no VMs in it yet — nothing else would ever touch it.
    //
    // Settles the VMs the previous run left active and resyncs host
    // networking. Best-effort: if the net-helper isn't up yet, the host side
    // lags until the next per-VM start, which re-applies the same thing (see
    // setup_vm_network) — not worth failing API startup over.
    let volume_state = state.clone();
    tokio::task::spawn_blocking(move || block_storage::recover(&volume_state))
        .await
        .map_err(|error| StartupError::VolumeRecovery(error.to_string()))?
        .map_err(StartupError::VolumeRecovery)?;
    reconcile::reconcile(&state).await;
    service_runtime::recover(&state).await;
    // Fetch the shared bootstrap builder source now, in the background, so
    // the request that needs it doesn't have to — see spawn_warmup.
    microboot::spawn_warmup(state.clone());
    // Keep warm pools filled after startup reconciliation has settled existing VMs.
    pool::spawn_reconciler(state.clone());

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .map_err(|source| StartupError::Bind {
            address: config.bind_addr,
            source,
        })?;

    let local_address = listener.local_addr().map_err(StartupError::LocalAddress)?;
    state.api_port = local_address.port();
    scheduler::spawn(state.clone(), scheduler_ownership)
        .await
        .map_err(StartupError::Persistence)?;
    let _git_sources = stack_source::spawn_reconciler(state.clone());
    let app = build_router(state, &config);
    let scheme = if transport.is_some() { "https" } else { "http" };
    tracing::info!(address = %local_address, %scheme, "listening on {scheme}://{local_address}");
    tls::serve(listener, app, transport)
        .await
        .map_err(StartupError::Serve)?;
    Ok(())
}
