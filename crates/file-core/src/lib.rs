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
#[cfg(feature = "images")]
mod storage_usage;

use std::{error::Error, time::Duration};
use tokio::net::TcpListener;

pub fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
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
    let job_scheduler = state.job_scheduler.clone();
    let activity = state.activity.clone();
    let ready_flag = lifecycle.ready_flag();
    let bind_address = config.bind_address.clone();
    let shutdown_timeout = Duration::from_secs(config.shutdown_timeout_seconds);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    #[cfg(feature = "images")]
    if let Some(catalog) = state.storage_catalog.clone() {
        runtime
            .block_on(storage_usage::register_jobs(
                &state.job_scheduler,
                catalog,
                state.storage_operator.clone(),
                state.storage_inventory_interval_seconds,
                state.activity.clone(),
                state.background_idle_seconds,
            ))
            .map_err(std::io::Error::other)?;
    }
    let app = router::build_router(state);
    let result = runtime.block_on(async move {
        let listener = TcpListener::bind(&bind_address).await?;
        let job_runtime = job_scheduler.start(activity, ready_flag);
        lifecycle.set_ready(true);
        tracing::info!(address = %bind_address, "image service listening");
        tracing::info!(backend = %config.storage.scheme, "image storage configured");
        let result = server::serve(listener, app, lifecycle, shutdown_timeout).await;
        job_runtime.shutdown(shutdown_timeout).await;
        result
    });
    runtime.shutdown_timeout(Duration::ZERO);
    if let Err(error) = &result {
        tracing::error!(error = %error, "image service stopped with an error");
    }
    result?;
    Ok(())
}

pub use job_scheduler as jobs;

#[cfg(feature = "worker-protocol")]
pub use worker_protocol as protocol;
