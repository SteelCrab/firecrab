mod artifacts;
mod bootstrap;
mod console;
mod error;
mod extract;
mod firecracker;
mod guest_agent;
mod handlers;
mod image_install;
mod ipam;
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
use tokio::net::TcpListener;

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

    HttpListeners::bind(config.bind_addr, config.benchmark_bind_addr)
        .await?
        .serve(app)
        .await
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
