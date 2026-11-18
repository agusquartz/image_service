#[cfg(feature = "images")]
use crate::storage_usage::{get_usage, request_reconciliation};
#[cfg(feature = "images")]
use crate::{
    config::MULTIPART_OVERHEAD_BYTES,
    images::handler::{
        create_image, delete_image, delete_resource, fetch_image, head_image, image_info,
        list_images, presign_image_read, replace_image,
    },
};
use crate::{
    health::{health, live, ready},
    observability::{metrics, observe_request},
    state::AppState,
};

#[cfg(feature = "images")]
use axum::extract::DefaultBodyLimit;
#[cfg(feature = "images")]
use axum::routing::post;
use axum::{
    Router,
    extract::{Request, State},
    middleware,
    middleware::Next,
    response::Response,
    routing::get,
};

/// Builds the complete HTTP router for the image service.
///
/// Routes:
///
/// ```text
/// GET  /health
/// GET  /health/live
/// GET  /health/ready
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
    let metrics_enabled = state.metrics_enabled;
    let mut router = Router::new()
        .route("/health", get(health))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready));

    #[cfg(feature = "images")]
    {
        let max_request_bytes = state
            .max_request_source_bytes
            .saturating_add(MULTIPART_OVERHEAD_BYTES);
        router = router
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
            .layer(DefaultBodyLimit::max(max_request_bytes));
        if state.storage_usage_enabled {
            router = router
                .route("/api/storage/usage", get(get_usage))
                .route("/api/storage/reconcile", post(request_reconciliation));
        }
    }

    if metrics_enabled {
        router = router.route("/metrics", get(metrics));
    }

    router
        .layer(middleware::from_fn_with_state(
            state.clone(),
            track_foreground_activity,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            observe_request,
        ))
        .with_state(state)
}

async fn track_foreground_activity(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let _activity = if path.starts_with("/health") || path == "/metrics" {
        None
    } else {
        Some(state.activity.enter())
    };
    next.run(request).await
}
