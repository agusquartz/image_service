use axum::{
    http::StatusCode,
    response::{
        IntoResponse,
        Response,
    },
    Json,
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

    NotFound(String),

    UnsupportedMediaType(String),

    PayloadTooLarge(String),

    UnprocessableEntity(String),

    /// Filesystem/storage failure.
    ///
    /// The internal detail is logged but is intentionally not sent
    /// directly to the client.
    Io(String),

    /// Unexpected internal application failure.
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) =
            match self {
                Self::BadRequest(message) => (
                    StatusCode::BAD_REQUEST,
                    message,
                ),

                Self::Unauthorized => (
                    StatusCode::UNAUTHORIZED,
                    "Unauthorized".to_string(),
                ),

                Self::NotFound(message) => (
                    StatusCode::NOT_FOUND,
                    message,
                ),

                Self::UnsupportedMediaType(message) => (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    message,
                ),

                Self::PayloadTooLarge(message) => (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    message,
                ),

                Self::UnprocessableEntity(message) => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    message,
                ),

                Self::Io(detail) => {
                    eprintln!(
                        "Filesystem error: {detail}"
                    );

                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal storage error"
                            .to_string(),
                    )
                }

                Self::Internal(detail) => {
                    eprintln!(
                        "Internal error: {detail}"
                    );

                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal server error"
                            .to_string(),
                    )
                }
            };

        (
            status,
            Json(
                ErrorResponse {
                    error: message,
                }
            ),
        )
            .into_response()
    }
}
