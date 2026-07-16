mod error;
mod handlers;
mod model;
mod persistence;
mod state;
mod templates;

use std::error::Error;
use std::io;
use std::process::ExitCode;

use axum::Router;
use axum::routing::{get, post};
use persistence::PersistenceError;
use state::AppState;
use templates::{TemplateError, TemplateRegistry};
use thiserror::Error;
use tower_http::cors::{Any, CorsLayer};

const LISTEN_ADDRESS: &str = "0.0.0.0:3000";

#[derive(Debug, Error)]
enum StartupError {
    #[error("failed to initialize template registry")]
    Template(#[source] TemplateError),
    #[error("failed to load persisted VM state")]
    Persistence(#[source] PersistenceError),
    #[error("failed to bind API listener at {address}")]
    Bind {
        address: &'static str,
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

async fn run() -> Result<(), StartupError> {
    let templates = TemplateRegistry::load_default().map_err(StartupError::Template)?;
    let state = AppState::new(templates)
        .await
        .map_err(StartupError::Persistence)?;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/api/vms", get(handlers::vms::list_vms).post(handlers::vms::create_vm))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(LISTEN_ADDRESS)
        .await
        .map_err(|source| StartupError::Bind {
            address: LISTEN_ADDRESS,
            source,
        })?;

    let local_address = listener.local_addr().map_err(StartupError::LocalAddress)?;
    println!("[INFO] Listening on http://{local_address}");
    axum::serve(listener, app)
        .await
        .map_err(StartupError::Serve)?;
    Ok(())
}
