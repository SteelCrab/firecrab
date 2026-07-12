mod error;
mod extract;
mod handlers;
mod model;
mod persistence;
mod server;
mod state;
mod templates;

use server::{HttpConfig, build_router};
use state::AppState;
use templates::TemplateRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = HttpConfig::load()?;
    let state = AppState::new(TemplateRegistry::load_default()?);
    let app = build_router(state, &config);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;

    println!("[INFO] Listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}
