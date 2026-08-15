use crate::{
    config::MAX_REQUEST_BYTES,
    health::health,
    images::handler::{
        fetch_image,
        upload_image,
    },
    state::AppState,
};

use axum::{
    extract::DefaultBodyLimit,
    routing::get,
    Router,
};

/// Builds the complete HTTP router for the image service.
///
/// Routes:
///
/// ```text
/// GET  /health
///
/// GET  /api/images/{namespace}/{resource_id}/{slot}
/// POST /api/images/{namespace}/{resource_id}/{slot}
/// ```
pub fn build_router(
    state: AppState,
) -> Router {
    Router::new()
        .route(
            "/health",
            get(health),
        )
        .route(
            "/api/images/{namespace}/{resource_id}/{slot}",
            get(fetch_image)
                .post(upload_image),
        )

        // Multipart parsing is still constrained by Axum's body limit.
        //
        // The limit is intentionally slightly larger than the maximum
        // source image size because multipart requests include
        // additional metadata and boundary bytes.
        .layer(
            DefaultBodyLimit::max(
                MAX_REQUEST_BYTES
            )
        )

        .with_state(state)
}
