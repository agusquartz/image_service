#[cfg(feature = "images")]
use crate::image_jobs::{create_image_job, get_image_job_status, replace_image_job};
#[cfg(feature = "images")]
use crate::resumable_uploads::{
    create_post_session, create_put_session, delete_session, head_session, patch_session,
    tus_options,
};
#[cfg(feature = "images")]
use crate::storage_usage::{
    get_policy_audit, get_policy_audit_findings, get_usage, request_reconciliation,
    start_policy_audit,
};
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
    http::{HeaderName, HeaderValue, Method, header},
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
/// POST /api/image-jobs/{namespace}/{resource_id}/{slot} (async create)
/// PUT  /api/image-jobs/{namespace}/{resource_id}/{slot} (async create or replace)
/// GET  /api/image-jobs/{job_id}
/// POST/PUT /api/image-uploads/{namespace}/{resource_id}/{slot}
/// HEAD/PATCH/DELETE /api/image-uploads/{upload_id}
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
            .route(
                "/api/image-jobs/{namespace}/{resource_id}/{slot}",
                post(create_image_job).put(replace_image_job),
            )
            .route("/api/image-jobs/{job_id}", get(get_image_job_status));
        if state.resumable_uploads_enabled {
            router = router
                .route("/api/image-uploads", axum::routing::options(tus_options))
                .route(
                    "/api/image-uploads/{namespace}/{resource_id}/{slot}",
                    post(create_post_session).put(create_put_session),
                )
                .route(
                    "/api/image-uploads/{upload_id}",
                    axum::routing::head(head_session)
                        .patch(patch_session)
                        .delete(delete_session)
                        .options(tus_options),
                );
        }
        // Multipart and resumable PATCH bodies share a strict outer bound.
        // The extra framing allowance is needed by multipart POST/PUT.
        router = router.layer(DefaultBodyLimit::max(max_request_bytes));
        if state.storage_usage_enabled {
            router = router
                .route("/api/storage/usage", get(get_usage))
                .route("/api/storage/reconcile", post(request_reconciliation))
                .route("/api/storage/audits", post(start_policy_audit))
                .route("/api/storage/audits/{audit_id}", get(get_policy_audit))
                .route(
                    "/api/storage/audits/{audit_id}/findings",
                    get(get_policy_audit_findings),
                );
        }
    }

    if metrics_enabled {
        router = router.route("/metrics", get(metrics));
    }

    router
        .layer(middleware::from_fn(tus_response_headers))
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

async fn tus_response_headers(request: Request, next: Next) -> Response {
    let is_tus_request = request.uri().path().starts_with("/api/image-uploads")
        && request.method() != Method::OPTIONS;
    let mut response = next.run(request).await;
    if is_tus_request {
        response.headers_mut().insert(
            HeaderName::from_static("tus-resumable"),
            HeaderValue::from_static("1.0.0"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn track_foreground_activity(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    let is_audit_observation = path.starts_with("/api/storage/audits/");
    let _activity = if path.starts_with("/health") || path == "/metrics" || is_audit_observation {
        None
    } else {
        Some(state.activity.enter())
    };
    next.run(request).await
}
