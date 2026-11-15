// The Core-only variants intentionally compile image API configuration and
// error mappings for the workspace's default service build without using them.
#![cfg_attr(not(feature = "images"), allow(dead_code))]

mod auth;
mod config;
mod error;
mod health;
#[cfg(feature = "images")]
pub(crate) mod images;
mod observability;
mod rate_limit;
mod router;
mod server;
mod state;

use std::{error::Error, time::Duration};
use tokio::net::TcpListener;

pub fn run() -> Result<(), Box<dyn Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let config = config::AppConfig::from_env()?;
    let state = state::AppState::from_config(&config)?;
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

#[cfg(feature = "ipc")]
pub use image_worker_protocol as ipc_protocol;
