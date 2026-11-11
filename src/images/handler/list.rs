use super::super::{
    dtos::ImageListResponse,
    service,
    validation::{validate_namespace, validate_resource_id},
};
use crate::{auth::require_auth, error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};

pub(crate) async fn list_images(
    State(state): State<AppState>,
    Path((namespace, resource_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<ImageListResponse>, ApiError> {
    require_auth(&headers, state.read_auth, state.api_key.as_deref())?;
    validate_namespace(&namespace)?;
    validate_resource_id(&resource_id)?;
    Ok(Json(
        service::list_resource(&state, namespace, resource_id).await?,
    ))
}
