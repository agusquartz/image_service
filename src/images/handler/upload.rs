use crate::{auth::require_write_auth, error::ApiError, state::AppState};

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::HeaderMap,
};

use super::super::{
    dtos::UploadResponse, model::ImageKey, multipart::read_multipart_image, service,
};

/// Uploads or replaces an image.
///
/// # Endpoint
///
/// ```text
/// POST /api/images/{namespace}/{resource_id}/{slot}
/// ```
///
/// # Request
///
/// The request must use `multipart/form-data` and contain exactly one
/// image field named:
///
/// ```text
/// image
/// ```
///
/// # Authentication
///
/// If `IMAGE_API_KEY` is configured, the request must contain:
///
/// ```text
/// Authorization: Bearer <IMAGE_API_KEY>
/// ```
///
/// # Behavior
///
/// The image is:
///
/// 1. Validated.
/// 2. Decoded.
/// 3. Converted to WebP.
/// 4. Reduced until it satisfies the configured output size limit.
/// 5. Stored in the requested resource slot.
///
/// Uploading another image to the same slot replaces the existing file.
pub(crate) async fn upload_image(
    State(state): State<AppState>,

    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,

    headers: HeaderMap,

    multipart: Multipart,
) -> Result<Json<UploadResponse>, ApiError> {
    // Authorization happens before expensive image parsing and
    // processing.
    require_write_auth(&headers, state.api_key.as_deref())?;

    // Validate path parameters before consuming and processing a
    // potentially large multipart body.
    let key = ImageKey::new(namespace, resource_id, slot)?;

    let incoming = read_multipart_image(multipart).await?;

    let response = service::upload(&state, &key, incoming).await?;

    Ok(Json(response))
}
