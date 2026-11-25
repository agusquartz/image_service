use crate::{auth::require_auth, error::ApiError, state::AppState};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};
use futures_util::{StreamExt, TryStreamExt};
use std::time::SystemTime;

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
    require_auth(&headers, state.read_auth, state.api_key.as_deref())?;

    let key = ImageKey::new(namespace, resource_id, slot)?;

    // A matching storage validator can answer conditional requests without
    // opening a response body.
    if headers.contains_key(header::IF_NONE_MATCH)
        || headers.contains_key(header::IF_MODIFIED_SINCE)
    {
        let metadata = service::stat(&state, &key).await?;
        let etag = metadata.etag.clone().map(service::normalize_etag);
        if is_not_modified(&headers, etag.as_deref(), metadata.last_modified)
            && service::is_current_snapshot(&state, &key, &metadata).await?
        {
            return image_response(
                &state,
                Body::empty(),
                StatusCode::NOT_MODIFIED,
                etag.as_deref(),
                metadata.last_modified,
            );
        }
    }

    let (metadata, reader) = service::open_download(&state, &key).await?;
    let etag = metadata.etag.clone().map(service::normalize_etag);
    let length = metadata.content_length;
    if is_not_modified(&headers, etag.as_deref(), metadata.last_modified) {
        return image_response(
            &state,
            Body::empty(),
            StatusCode::NOT_MODIFIED,
            etag.as_deref(),
            metadata.last_modified,
        );
    }

    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .filter(|_| if_range_matches(&headers, etag.as_deref(), metadata.last_modified))
        .and_then(|value| parse_single_byte_range(value, length));

    let range = match range {
        Some(ParsedRange::Unsatisfiable) => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{length}"))
                    .map_err(|error| ApiError::Internal(error.to_string()))?,
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
            set_cache_headers(
                &state,
                response.headers_mut(),
                etag.as_deref(),
                metadata.last_modified,
            )?;
            return Ok(response);
        }
        range => range,
    };

    let (status, start, end, content_range) = match range {
        Some(ParsedRange::Satisfiable { start, end }) => {
            let end_exclusive = end
                .checked_add(1)
                .ok_or_else(|| ApiError::BadRequest("Requested byte range is too large".into()))?;
            (
                StatusCode::PARTIAL_CONTENT,
                start,
                end_exclusive,
                Some(format!("bytes {start}-{end}/{length}")),
            )
        }
        None => (StatusCode::OK, 0, length, None),
        Some(ParsedRange::Unsatisfiable) => unreachable!("handled above"),
    };

    let content_length = end.saturating_sub(start);
    let permit = state.try_admit_image_download()?;
    let stream = reader
        .into_stream(start..end)
        .await
        .map_err(|error| ApiError::Storage(error.to_string()))?
        .map_ok(|buffer| buffer.to_bytes())
        .map_err(std::io::Error::other)
        .map(move |item| {
            let _permit = &permit;
            item
        });
    let mut response = image_response(
        &state,
        Body::from_stream(stream),
        status,
        etag.as_deref(),
        metadata.last_modified,
    )?;
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string())
            .map_err(|error| ApiError::Internal(error.to_string()))?,
    );
    if let Some(content_range) = content_range {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range)
                .map_err(|error| ApiError::Internal(error.to_string()))?,
        );
    }
    Ok(response)
}

fn image_response(
    state: &AppState,
    body: Body,
    status: StatusCode,
    etag: Option<&str>,
    last_modified: Option<SystemTime>,
) -> Result<Response, ApiError> {
    let mut response = Response::new(body);
    *response.status_mut() = status;

    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));

    // Prevent browsers from attempting to reinterpret the returned
    // resource as another content type.
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    set_cache_headers(state, response.headers_mut(), etag, last_modified)?;

    Ok(response)
}

/// Returns existence and content length without reading image bytes.
pub(crate) async fn head_image(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_auth(&headers, state.read_auth, state.api_key.as_deref())?;
    let key = ImageKey::new(namespace, resource_id, slot)?;
    let metadata = service::stat(&state, &key).await?;
    let etag = metadata.etag.map(service::normalize_etag);
    let not_modified = is_not_modified(&headers, etag.as_deref(), metadata.last_modified);
    let mut response = Response::new(Body::empty());
    *response.status_mut() = if not_modified {
        StatusCode::NOT_MODIFIED
    } else {
        StatusCode::OK
    };
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if !not_modified {
        let content_length = HeaderValue::from_str(&metadata.content_length.to_string())
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, content_length);
    }
    set_cache_headers(
        &state,
        response.headers_mut(),
        etag.as_deref(),
        metadata.last_modified,
    )?;
    Ok(response)
}

enum ParsedRange {
    Unsatisfiable,
    Satisfiable { start: u64, end: u64 },
}

/// Returns a parsed single byte range. Unsupported or malformed range syntax
/// is ignored as required for a server that does not support that range set.
fn parse_single_byte_range(value: &str, length: u64) -> Option<ParsedRange> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 || length == 0 {
            return Some(ParsedRange::Unsatisfiable);
        }
        let start = length.saturating_sub(suffix);
        return Some(ParsedRange::Satisfiable {
            start,
            end: length - 1,
        });
    }
    let start = start.parse::<u64>().ok()?;
    if start >= length {
        return Some(ParsedRange::Unsatisfiable);
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        let end = end.parse::<u64>().ok()?;
        if end < start {
            return Some(ParsedRange::Unsatisfiable);
        }
        end.min(length - 1)
    };
    Some(ParsedRange::Satisfiable { start, end })
}

fn if_range_matches(
    headers: &HeaderMap,
    etag: Option<&str>,
    last_modified: Option<SystemTime>,
) -> bool {
    let Some(value) = headers.get(header::IF_RANGE) else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    if value.starts_with('"') {
        return etag.is_some_and(|etag| !etag.starts_with("W/") && value == etag);
    }
    let (Some(last_modified), Ok(date)) = (last_modified, httpdate::parse_http_date(value)) else {
        return false;
    };
    httpdate::parse_http_date(&httpdate::fmt_http_date(last_modified))
        .is_ok_and(|modified| modified <= date)
}

fn is_not_modified(
    headers: &HeaderMap,
    current_etag: Option<&str>,
    last_modified: Option<SystemTime>,
) -> bool {
    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH) {
        let Ok(if_none_match) = if_none_match.to_str() else {
            return false;
        };

        return if_none_match.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*"
                || current_etag
                    .is_some_and(|current| strip_weak_etag(candidate) == strip_weak_etag(current))
        });
    }

    let (Some(if_modified_since), Some(last_modified)) =
        (headers.get(header::IF_MODIFIED_SINCE), last_modified)
    else {
        return false;
    };

    if let Ok(if_modified_since) = if_modified_since.to_str()
        && let Ok(if_modified_since) = httpdate::parse_http_date(if_modified_since)
    {
        let last_modified = httpdate::parse_http_date(&httpdate::fmt_http_date(last_modified))
            .unwrap_or(last_modified);
        return last_modified <= if_modified_since;
    }

    false
}

fn strip_weak_etag(etag: &str) -> &str {
    etag.strip_prefix("W/").unwrap_or(etag).trim()
}

fn set_cache_headers(
    state: &AppState,
    headers: &mut HeaderMap,
    etag: Option<&str>,
    last_modified: Option<SystemTime>,
) -> Result<(), ApiError> {
    if let Some(etag) = etag
        && let Ok(value) = HeaderValue::from_str(etag)
    {
        headers.insert(header::ETAG, value);
    }

    if let Some(last_modified) = last_modified {
        let value = HeaderValue::from_str(&httpdate::fmt_http_date(last_modified))
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        headers.insert(header::LAST_MODIFIED, value);
    }

    let value = HeaderValue::from_str(&state.cache_control)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    headers.insert(header::CACHE_CONTROL, value);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_strong_and_weak_if_none_match_validators() {
        let strong =
            HeaderMap::from_iter([(header::IF_NONE_MATCH, HeaderValue::from_static("\"abc\""))]);
        let weak =
            HeaderMap::from_iter([(header::IF_NONE_MATCH, HeaderValue::from_static("W/\"abc\""))]);

        assert!(is_not_modified(&strong, Some("\"abc\""), None));
        assert!(is_not_modified(&weak, Some("\"abc\""), None));
    }

    #[test]
    fn wildcard_matches_existing_image_without_storage_etag() {
        let headers =
            HeaderMap::from_iter([(header::IF_NONE_MATCH, HeaderValue::from_static("*"))]);

        assert!(is_not_modified(&headers, None, None));
    }

    #[test]
    fn if_none_match_takes_precedence_over_if_modified_since() {
        let now = SystemTime::now();
        let headers = HeaderMap::from_iter([
            (header::IF_NONE_MATCH, HeaderValue::from_static("\"old\"")),
            (
                header::IF_MODIFIED_SINCE,
                HeaderValue::from_str(&httpdate::fmt_http_date(
                    now + std::time::Duration::from_secs(60),
                ))
                .expect("HTTP date should be a valid header"),
            ),
        ]);

        assert!(!is_not_modified(&headers, Some("\"new\""), Some(now)));
    }

    #[test]
    fn if_modified_since_matches_http_date_precision() {
        let last_modified = SystemTime::now();
        let headers = HeaderMap::from_iter([(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_str(&httpdate::fmt_http_date(last_modified))
                .expect("HTTP date should be a valid header"),
        )]);

        assert!(is_not_modified(&headers, None, Some(last_modified)));
    }
}
