use crate::error::ApiError;

use axum::{
    http::{
        header,
        HeaderMap,
    },
};

/// Verifies authorization for write operations.
///
/// Authentication is optional:
///
/// - If no API key is configured, the operation is allowed.
/// - If an API key exists, the request must contain:
///
///   `Authorization: Bearer <api-key>`
///
/// This function is intended for mutation handlers such as uploads.
/// Public image reads do not require it.
pub fn require_write_auth(
    headers: &HeaderMap,
    api_key: Option<&str>,
) -> Result<(), ApiError> {
    let Some(api_key) = api_key else {
        return Ok(());
    };

    let authorization =
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| {
                value.to_str().ok()
            })
            .ok_or(ApiError::Unauthorized)?;

    let expected =
        format!("Bearer {api_key}");

    if authorization != expected {
        return Err(
            ApiError::Unauthorized
        );
    }

    Ok(())
}
