mod auth;
mod config;
mod error;
mod health;
mod images;
mod observability;
mod rate_limit;
mod router;
mod server;
mod state;

use config::AppConfig;
use state::AppState;

use std::{error::Error, time::Duration};

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
fn main() -> Result<(), Box<dyn Error>> {
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
    let lifecycle = state.lifecycle.clone();
    let app = router::build_router(state);
    let bind_address = config.bind_address.clone();
    let shutdown_timeout = Duration::from_secs(config.shutdown_timeout_seconds);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async move {
        let listener = TcpListener::bind(&bind_address).await?;
        lifecycle.set_ready(true);
        tracing::info!(address = %bind_address, "image service listening");
        tracing::info!(backend = %config.storage.scheme, "image storage configured");
        server::serve(listener, app, lifecycle, shutdown_timeout).await
    });
    runtime.shutdown_timeout(Duration::ZERO);
    if let Err(error) = &result {
        tracing::error!(error = %error, "image service stopped with an error");
    }
    result?;

    Ok(())
}
