use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use serde::Serialize;

/// Standard JSON error returned by the API.
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// Errors that can be produced while serving an API request.
///
/// Keeping application errors in one enum makes it possible to map
/// internal failures to consistent HTTP responses.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),

    Unauthorized,

    RateLimited(u64),

    Conflict(String),

    NotFound(String),

    UnsupportedMediaType(String),

    NotImplemented(String),

    PayloadTooLarge(String),

    RequestTimeout(String),

    ServiceUnavailable(String),

    UnprocessableEntity(String),

    /// Filesystem/storage failure.
    ///
    /// The internal detail is logged but is intentionally not sent
    /// directly to the client.
    Storage(String),

    /// Unexpected internal application failure.
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),

            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()),

            Self::RateLimited(retry_after) => {
                let mut response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(ErrorResponse {
                        error: "Rate limit exceeded".to_string(),
                    }),
                )
                    .into_response();
                if let Ok(value) = retry_after.to_string().parse() {
                    response
                        .headers_mut()
                        .insert(axum::http::header::RETRY_AFTER, value);
                }
                return response;
            }

            Self::Conflict(message) => (StatusCode::CONFLICT, message),

            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),

            Self::UnsupportedMediaType(message) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, message),

            Self::NotImplemented(message) => (StatusCode::NOT_IMPLEMENTED, message),

            Self::PayloadTooLarge(message) => (StatusCode::PAYLOAD_TOO_LARGE, message),

            Self::RequestTimeout(message) => (StatusCode::REQUEST_TIMEOUT, message),

            Self::ServiceUnavailable(message) => {
                let mut response = (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ErrorResponse { error: message }),
                )
                    .into_response();
                response.headers_mut().insert(
                    axum::http::header::RETRY_AFTER,
                    axum::http::HeaderValue::from_static("1"),
                );
                return response;
            }

            Self::UnprocessableEntity(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),

            Self::Storage(detail) => {
                tracing::error!(error = %detail, "storage operation failed");

                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal storage error".to_string(),
                )
            }

            Self::Internal(detail) => {
                tracing::error!(error = %detail, "internal operation failed");

                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
        };

        (status, Json(ErrorResponse { error: message })).into_response()
    }
}
