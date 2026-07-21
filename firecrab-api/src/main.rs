mod error;
mod extract;
mod firecracker;
mod handlers;
mod ipam;
mod model;
mod network_policy;
mod persistence;
mod rootfs;
mod server;
mod state;
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
