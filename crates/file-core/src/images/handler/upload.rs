use crate::{auth::require_auth, error::ApiError, state::AppState};

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
};

use super::super::{
    dtos::UploadResponse, model::ImageKey, multipart::read_multipart_image, service,
};

/// Creates an image in an unused slot.
pub(crate) async fn create_image(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let (response, _) = upload_image(
        state,
        namespace,
        resource_id,
        slot,
        headers,
        multipart,
        false,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Creates or replaces the complete image in a slot.
pub(crate) async fn replace_image(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<(StatusCode, Json<UploadResponse>), ApiError> {
    let (response, was_created) = upload_image(
        state,
        namespace,
        resource_id,
        slot,
        headers,
        multipart,
        true,
    )
    .await?;
    let status = if was_created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(response)))
}

async fn upload_image(
    state: AppState,
    namespace: String,
    resource_id: String,
    slot: u8,
    headers: HeaderMap,
    multipart: Multipart,
    replace_existing: bool,
) -> Result<(UploadResponse, bool), ApiError> {
    // Authorize before expensive multipart parsing and image processing.
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;

    // Validate the path before consuming a potentially large request body.
    let key = ImageKey::new(namespace, resource_id, slot)?;
    let policy = state.processing_policy_for(&key.namespace);

    // The token records the resource/slot generation before any upload body
    // work begins. A concurrent delete can then invalidate this upload.
    let operation = state.store.begin_operation(&key).await;

    if slot > state.max_slots {
        if !replace_existing {
            return Err(ApiError::BadRequest(format!(
                "cannot create slot {slot}: IMAGE_MAX_SLOTS is {}",
                state.max_slots
            )));
        }

        match state.store.stat(&key).await {
            Ok(_) => {}
            Err(ApiError::NotFound(_)) => {
                return Err(ApiError::BadRequest(format!(
                    "cannot create slot {slot}: IMAGE_MAX_SLOTS is {}",
                    state.max_slots
                )));
            }
            Err(error) => return Err(error),
        }
    }

    if !replace_existing {
        state.store.ensure_absent(&key).await?;
    }

    // Reserve compressed-body capacity before consuming the multipart stream.
    let admission = state.admit_upload(policy.max_input_bytes)?;
    let incoming = tokio::time::timeout(
        std::time::Duration::from_secs(state.upload_read_timeout_seconds),
        read_multipart_image(multipart, policy.max_input_bytes),
    )
    .await
    .map_err(|_| ApiError::RequestTimeout("Upload body read timed out".to_string()))??;

    service::upload(
        &state,
        &key,
        incoming,
        replace_existing,
        admission,
        operation,
    )
    .await
}
