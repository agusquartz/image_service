use crate::state::AppState;

use axum::{extract::State, http::StatusCode};
use std::time::Duration;

/// Backwards-compatible alias for the liveness probe.
///
/// A `204 No Content` response indicates that the HTTP process is running.
pub async fn health() -> StatusCode {
    live().await
}

/// Liveness does not depend on storage, so a storage outage does not restart
/// an otherwise responsive process.
pub async fn live() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// Readiness requires that the service is accepting work and storage responds.
pub async fn ready(State(state): State<AppState>) -> StatusCode {
    if !state.lifecycle.is_ready() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }

    match tokio::time::timeout(Duration::from_secs(2), state.storage_operator.check()).await {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        Ok(Err(error)) => {
            tracing::debug!(error = ?error, "storage readiness check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
        Err(_) => {
            tracing::debug!("storage readiness check timed out");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
