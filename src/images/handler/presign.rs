use super::super::{dtos::PresignResponse, model::ImageKey, service};
use crate::{auth::require_read_auth, error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};

pub(crate) async fn presign_image_read(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
) -> Result<Json<PresignResponse>, ApiError> {
    require_read_auth(&headers, state.getmethod_auth, state.api_key.as_deref())?;
    let key = ImageKey::new(namespace, resource_id, slot)?;
    Ok(Json(service::presign_read(&state, &key).await?))
}
