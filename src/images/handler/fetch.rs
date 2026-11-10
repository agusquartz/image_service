use crate::{auth::require_read_auth, error::ApiError, state::AppState};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};

use super::super::{model::ImageKey, service};

/// Retrieves a stored image.
///
/// # Endpoint
///
/// ```text
/// GET /api/images/{namespace}/{resource_id}/{slot}
/// ```
///
/// Images are always returned as:
///
/// ```text
/// Content-Type: image/webp
/// ```
///
/// because every source image is converted before being stored.
pub(crate) async fn fetch_image(
    State(state): State<AppState>,

    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,

    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_read_auth(&headers, state.getmethod_auth, state.api_key.as_deref())?;

    let key = ImageKey::new(namespace, resource_id, slot)?;

    let bytes = service::fetch(&state, &key).await?;

    let mut response = Response::new(Body::from(bytes));

    *response.status_mut() = StatusCode::OK;

    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));

    // Prevent browsers from attempting to reinterpret the returned
    // resource as another content type.
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );

    Ok(response)
}

/// Returns existence and content length without reading image bytes.
pub(crate) async fn head_image(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_read_auth(&headers, state.getmethod_auth, state.api_key.as_deref())?;
    let key = ImageKey::new(namespace, resource_id, slot)?;
    let size = service::stat(&state, &key).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    let content_length = HeaderValue::from_str(&size.to_string())
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, content_length);
    Ok(response)
}
