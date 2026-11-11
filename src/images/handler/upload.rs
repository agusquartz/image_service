use crate::{auth::require_write_auth, error::ApiError, state::AppState};

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
    require_write_auth(&headers, state.api_key.as_deref())?;

    // Validate the path before consuming a potentially large request body.
    let key = ImageKey::new(namespace, resource_id, slot)?;
    let policy = state.processing_policy_for(&key.namespace);

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

    let incoming = read_multipart_image(multipart, policy.max_input_bytes).await?;

    service::upload(&state, &key, incoming, replace_existing).await
}
