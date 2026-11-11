use super::super::{
    service,
    validation::{validate_namespace, validate_resource_id},
};
use crate::{auth::require_auth, error::ApiError, state::AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};

pub(crate) async fn delete_resource(
    State(state): State<AppState>,
    Path((namespace, resource_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    validate_namespace(&namespace)?;
    validate_resource_id(&resource_id)?;
    service::delete_resource(&state, &namespace, &resource_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
