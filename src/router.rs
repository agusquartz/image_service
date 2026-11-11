use crate::{
    config::MULTIPART_OVERHEAD_BYTES,
    health::health,
    images::handler::{
        create_image, delete_image, delete_resource, fetch_image, head_image, image_info,
        list_images, presign_image_read, replace_image,
    },
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
/// GET  /api/images/{namespace}/{resource_id}/{slot}/info
/// POST /api/images/{namespace}/{resource_id}/{slot} (create only)
/// PUT  /api/images/{namespace}/{resource_id}/{slot} (create or replace)
/// POST /api/images/{namespace}/{resource_id}/{slot}/presign
/// HEAD /api/images/{namespace}/{resource_id}/{slot}
/// DELETE /api/images/{namespace}/{resource_id}/{slot}
/// GET  /api/images/{namespace}/{resource_id}
/// DELETE /api/images/{namespace}/{resource_id}
/// ```
pub fn build_router(state: AppState) -> Router {
    let max_request_bytes = state
        .max_request_source_bytes
        .saturating_add(MULTIPART_OVERHEAD_BYTES);
    Router::new()
        .route("/health", get(health))
        .route(
            "/api/images/{namespace}/{resource_id}",
            get(list_images).delete(delete_resource),
        )
        .route(
            "/api/images/{namespace}/{resource_id}/{slot}/info",
            get(image_info),
        )
        .route(
            "/api/images/{namespace}/{resource_id}/{slot}/presign",
            axum::routing::post(presign_image_read),
        )
        .route(
            "/api/images/{namespace}/{resource_id}/{slot}",
            get(fetch_image)
                .head(head_image)
                .post(create_image)
                .put(replace_image)
                .delete(delete_image),
        )
        // Multipart parsing is still constrained by Axum's body limit.
        //
        // The limit is intentionally slightly larger than the maximum
        // source image size because multipart requests include
        // additional metadata and boundary bytes.
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .with_state(state)
}
