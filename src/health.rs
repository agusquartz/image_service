use axum::http::StatusCode;

/// Lightweight health endpoint.
///
/// A `204 No Content` response indicates that the HTTP service is
/// running.
pub async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}
