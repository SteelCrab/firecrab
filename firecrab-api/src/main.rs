mod handlers;
mod model;
mod persistence;
mod state;

use axum::routing::post;
use axum::Router;
use state::AppState;
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() {
    let state = AppState::new();

    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let app = Router::new()
        .route("/api/vms", post(handlers::vms::create_vm))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();

    println!("[INFO] Listening on http://{}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
