//! Firecrab API process startup and dual-listener ownership.

mod artifacts;
mod benchmark_jobs;
mod bootstrap;
mod console;
mod console_replay;
mod console_session;
mod error;
mod extract;
mod firecracker;
mod guest_agent;
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
mod process_metrics;
mod reconcile;
mod resource_limits;
mod rootfs;
mod server;
mod service_runtime;
mod shells;
mod state;
mod storage;
mod templates;
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
use tokio::net::TcpListener;

#[derive(Debug, Error)]
/// Failure that prevents the API process from serving requests.
enum StartupError {
    /// HTTP listener configuration could not be loaded.
    #[error("failed to load HTTP configuration")]
    Config(#[source] ConfigError),
    /// Installed VM templates could not be indexed.
    #[error("failed to initialize template registry")]
    Template(#[source] TemplateError),
    /// Persisted control-plane state could not be opened.
    #[error("failed to load persisted VM state")]
    Persistence(#[source] PersistenceError),
    /// One configured listener address could not be bound.
    #[error("failed to bind API listener at {address}")]
    Bind {
        /// Listener address that failed.
        address: SocketAddr,
        /// Operating-system bind error.
        #[source]
        source: io::Error,
    },
    /// A bound listener's effective address could not be read.
    #[error("failed to inspect API listener address")]
    LocalAddress(#[source] io::Error),
    /// An Axum listener terminated unexpectedly.
    #[error("API server terminated with an error")]
    Serve(#[source] io::Error),
}

/// Management/API and benchmark dashboard listeners owned by one process.
struct HttpListeners {
    /// Management dashboard, REST API, and console listener.
    api: TcpListener,
    /// Benchmark-first dashboard listener.
    benchmark: TcpListener,
}

impl HttpListeners {
    /// Binds both addresses before either server starts accepting requests.
    async fn bind(
        api_address: SocketAddr,
        benchmark_address: SocketAddr,
    ) -> Result<Self, StartupError> {
        let api = TcpListener::bind(api_address)
            .await
            .map_err(|source| StartupError::Bind {
                address: api_address,
                source,
            })?;
        let benchmark = TcpListener::bind(benchmark_address)
            .await
            .map_err(|source| StartupError::Bind {
                address: benchmark_address,
                source,
            })?;
        Ok(Self { api, benchmark })
    }

    /// Serves the same application router on both listeners.
    async fn serve(self, app: axum::Router) -> Result<(), StartupError> {
        let api_address = self.api.local_addr().map_err(StartupError::LocalAddress)?;
        let benchmark_address = self
            .benchmark
            .local_addr()
            .map_err(StartupError::LocalAddress)?;
        tracing::info!(address = %api_address, "management API listening on http://{api_address}");
        tracing::info!(address = %benchmark_address, "benchmark dashboard listening on http://{benchmark_address}");

        let api_app = app.clone();
        let api_server = async { axum::serve(self.api, api_app).await };
        let benchmark_server = async { axum::serve(self.benchmark, app).await };
        tokio::try_join!(api_server, benchmark_server).map_err(StartupError::Serve)?;
        Ok(())
    }
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

/// Installs the process-wide tracing subscriber.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "firecrab_api=info".into()),
        )
        .init();
}

/// Loads persistent state and serves both dashboard listeners.
async fn run() -> Result<(), StartupError> {
    init_tracing();
    let config = HttpConfig::load().map_err(StartupError::Config)?;
    let templates = TemplateRegistry::load_default().map_err(StartupError::Template)?;
    let mut state = AppState::new(templates)
        .await
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
    reconcile::reconcile(&state).await;
    service_runtime::recover(&state).await;
    // Fetch the shared bootstrap builder source now, in the background, so
    // the request that needs it doesn't have to — see spawn_warmup.
    microboot::spawn_warmup(state.clone());

    let listeners = HttpListeners::bind(config.bind_addr, config.benchmark_bind_addr).await?;
    state.api_port = listeners
        .api
        .local_addr()
        .map_err(StartupError::LocalAddress)?
        .port();
    let app = build_router(state, &config);
    listeners.serve(app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;

    #[tokio::test]
    async fn both_http_listeners_serve_the_same_application() {
        let listeners = HttpListeners::bind(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            SocketAddr::from(([127, 0, 0, 1], 0)),
        )
        .await
        .unwrap();
        let addresses = [
            listeners.api.local_addr().unwrap(),
            listeners.benchmark.local_addr().unwrap(),
        ];
        let app = axum::Router::new().route("/", get(|| async { "firecrab" }));
        let task = tokio::spawn(listeners.serve(app));

        for address in addresses {
            let response = reqwest::get(format!("http://{address}/")).await.unwrap();
            assert_eq!(response.text().await.unwrap(), "firecrab");
        }

        task.abort();
    }

    #[tokio::test]
    async fn benchmark_bind_failure_identifies_its_address() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = occupied.local_addr().unwrap();
        let result = HttpListeners::bind(SocketAddr::from(([127, 0, 0, 1], 0)), address).await;
        assert!(matches!(
            result,
            Err(StartupError::Bind {
                address: failed,
                ..
            }) if failed == address
        ));
    }
}
