use super::super::{model::ImageKey, service};
use crate::{auth::require_auth, error::ApiError, state::AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

pub(crate) async fn delete_image(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    let key = ImageKey::new(namespace, resource_id, slot)?;
    service::delete(&state, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}
