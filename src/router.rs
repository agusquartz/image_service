use crate::{
    config::MAX_REQUEST_BYTES,
    health::health,
    images::handler::{delete_image, fetch_image, head_image, list_images, upload_image},
    state::AppState,
};

use axum::{Router, extract::DefaultBodyLimit, routing::get};

/// Builds the complete HTTP router for the image service.
///
/// Routes:
///
/// ```text
/// GET  /health
///
/// GET  /api/images/{namespace}/{resource_id}/{slot}
/// POST /api/images/{namespace}/{resource_id}/{slot}
/// HEAD /api/images/{namespace}/{resource_id}/{slot}
/// DELETE /api/images/{namespace}/{resource_id}/{slot}
/// GET  /api/images/{namespace}/{resource_id}
/// ```
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/images/{namespace}/{resource_id}", get(list_images))
        .route(
            "/api/images/{namespace}/{resource_id}/{slot}",
            get(fetch_image)
                .head(head_image)
                .post(upload_image)
                .delete(delete_image),
        )
        // Multipart parsing is still constrained by Axum's body limit.
        //
        // The limit is intentionally slightly larger than the maximum
        // source image size because multipart requests include
        // additional metadata and boundary bytes.
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state)
}
