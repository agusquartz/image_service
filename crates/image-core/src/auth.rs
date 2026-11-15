use crate::{config::AuthRequirement, error::ApiError};

use axum::http::{HeaderMap, header};

/// Verifies a request against the configured authentication requirement.
pub fn require_auth(
    headers: &HeaderMap,
    requirement: AuthRequirement,
    api_key: Option<&str>,
) -> Result<(), ApiError> {
    if requirement == AuthRequirement::None {
        return Ok(());
    }

    let Some(api_key) = api_key else {
        tracing::error!("authentication is required but no API key is configured");
        return Err(ApiError::Internal(
            "Authentication is misconfigured".to_string(),
        ));
    };

    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let Some(provided_key) = authorization.strip_prefix("Bearer ") else {
        return Err(ApiError::Unauthorized);
    };

    let matches = bool::from(subtle::ConstantTimeEq::ct_eq(
        provided_key.as_bytes(),
        api_key.as_bytes(),
    ));
    if provided_key.len() != api_key.len() || !matches {
        return Err(ApiError::Unauthorized);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_auth_rejects_missing_and_incorrect_bearer_tokens() {
        let requirement = AuthRequirement::Required;
        assert!(require_auth(&HeaderMap::new(), requirement, Some("secret")).is_err());

        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer incorrect".parse().unwrap());
        assert!(require_auth(&headers, requirement, Some("secret")).is_err());
    }

    #[test]
    fn required_auth_accepts_the_configured_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer secret".parse().unwrap());
        assert!(require_auth(&headers, AuthRequirement::Required, Some("secret")).is_ok());
    }

    #[test]
    fn disabled_auth_does_not_require_a_token() {
        assert!(require_auth(&HeaderMap::new(), AuthRequirement::None, None).is_ok());
    }
}
