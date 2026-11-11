mod auth;
mod config;
mod error;
mod health;
mod images;
mod observability;
mod rate_limit;
mod router;
mod state;

use config::AppConfig;
use state::AppState;

use std::{error::Error, net::SocketAddr};

use tokio::net::TcpListener;

/// Application entry point.
///
/// Responsibilities here are intentionally limited to:
///
/// - Loading environment variables.
/// - Building the application configuration.
/// - Preparing the image storage directory.
/// - Creating the shared application state.
/// - Building the Axum router.
/// - Starting the HTTP server.
///
/// Business logic should not be added to this file.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let config = AppConfig::from_env()?;

    let state = AppState::from_config(&config)?;

    let app = router::build_router(state);

    let listener = TcpListener::bind(&config.bind_address).await?;

    tracing::info!(address = %config.bind_address, "image service listening");
    tracing::info!(backend = %config.storage.scheme, "image storage configured");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
