use crate::error::ApiError;

use axum::extract::Multipart;
use opendal::Writer;
use sha2::{Digest, Sha256};

use super::model::IncomingImage;

pub(crate) struct StagedImageInfo {
    pub source_bytes: usize,
    pub mime: Option<String>,
    pub content_sha256: String,
}

/// Extracts exactly one `image` field from a multipart request.
///
/// Expected form:
///
/// ```text
/// Content-Type: multipart/form-data
///
/// image=<binary image>
/// ```
///
/// Additional multipart fields are ignored.
///
/// The request is rejected if:
///
/// - The `image` field is missing.
/// - Multiple `image` fields are provided.
/// - The image is empty.
/// - The image exceeds the configured source size limit.
pub(crate) async fn read_multipart_image(
    mut multipart: Multipart,
    max_source_bytes: usize,
) -> Result<IncomingImage, ApiError> {
    let mut image: Option<IncomingImage> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge(error.body_text())
        } else {
            ApiError::BadRequest(format!("Invalid multipart request: {error}"))
        }
    })? {
        let name = field.name().unwrap_or("").to_string();

        if name != "image" {
            continue;
        }

        if image.is_some() {
            return Err(ApiError::BadRequest(
                "Only one 'image' field is allowed".to_string(),
            ));
        }

        let mime = field.content_type().map(str::to_string);

        let mut bytes = Vec::new();
        let mut type_prefix = Vec::with_capacity(32);
        let mut format_checked = false;
        while let Some(chunk) = field.chunk().await.map_err(|error| {
            if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::PayloadTooLarge(error.body_text())
            } else {
                ApiError::BadRequest(format!("Could not read image data: {error}"))
            }
        })? {
            if chunk.len() > max_source_bytes.saturating_sub(bytes.len()) {
                return Err(ApiError::PayloadTooLarge(format!(
                    "The source image exceeds the {} MB limit",
                    max_source_bytes.div_ceil(1024 * 1024)
                )));
            }
            if !format_checked && type_prefix.len() < 32 {
                let prefix_len = (32 - type_prefix.len()).min(chunk.len());
                type_prefix.extend_from_slice(&chunk[..prefix_len]);
                if let Ok(format) = image::guess_format(&type_prefix) {
                    if !super::reduction::supported_format(format)
                        || !super::reduction::declared_mime_is_acceptable(mime.as_deref(), format)
                    {
                        return Err(ApiError::UnsupportedMediaType(format!(
                            "Declared Content-Type does not match a supported image format ({format:?})"
                        )));
                    }
                    format_checked = true;
                }
            }
            bytes.extend_from_slice(&chunk);
        }

        if bytes.is_empty() {
            return Err(ApiError::BadRequest(
                "The uploaded image is empty".to_string(),
            ));
        }

        image = Some(IncomingImage {
            bytes: bytes.into(),
            mime,
        });
    }

    image.ok_or_else(|| ApiError::BadRequest("Missing multipart field 'image'".to_string()))
}

/// Streams a multipart image directly to its private staging writer while
/// enforcing the same byte and detected-format checks as the synchronous path.
pub(crate) async fn stage_multipart_image(
    mut multipart: Multipart,
    max_source_bytes: usize,
    mut writer: Writer,
) -> Result<StagedImageInfo, ApiError> {
    let mut seen_image = false;
    let mut source_bytes = 0_usize;
    let mut type_prefix = Vec::with_capacity(32);
    let mut format_checked = false;
    let mut mime = None;
    let mut hasher = Sha256::new();

    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge(error.body_text())
        } else {
            ApiError::BadRequest(format!("Invalid multipart request: {error}"))
        }
    })? {
        if field.name().unwrap_or("") != "image" {
            continue;
        }
        if seen_image {
            return Err(ApiError::BadRequest(
                "Only one 'image' field is allowed".to_string(),
            ));
        }
        seen_image = true;
        mime = field.content_type().map(str::to_string);

        while let Some(chunk) = field.chunk().await.map_err(|error| {
            if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::PayloadTooLarge(error.body_text())
            } else {
                ApiError::BadRequest(format!("Could not read image data: {error}"))
            }
        })? {
            if chunk.len() > max_source_bytes.saturating_sub(source_bytes) {
                return Err(ApiError::PayloadTooLarge(format!(
                    "The source image exceeds the {} MB limit",
                    max_source_bytes.div_ceil(1024 * 1024)
                )));
            }
            if !format_checked && type_prefix.len() < 32 {
                let prefix_len = (32 - type_prefix.len()).min(chunk.len());
                type_prefix.extend_from_slice(&chunk[..prefix_len]);
                if let Ok(format) = image::guess_format(&type_prefix) {
                    if !super::reduction::supported_format(format)
                        || !super::reduction::declared_mime_is_acceptable(mime.as_deref(), format)
                    {
                        return Err(ApiError::UnsupportedMediaType(format!(
                            "Declared Content-Type does not match a supported image format ({format:?})"
                        )));
                    }
                    format_checked = true;
                }
            }
            writer
                .write(chunk.clone())
                .await
                .map_err(|error| ApiError::Storage(error.to_string()))?;
            hasher.update(&chunk);
            source_bytes = source_bytes.saturating_add(chunk.len());
        }
    }

    if !seen_image {
        return Err(ApiError::BadRequest(
            "Missing multipart field 'image'".to_string(),
        ));
    }
    if source_bytes == 0 {
        return Err(ApiError::BadRequest(
            "The uploaded image is empty".to_string(),
        ));
    }
    writer
        .close()
        .await
        .map_err(|error| ApiError::Storage(error.to_string()))?;
    let content_sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(StagedImageInfo {
        source_bytes,
        mime,
        content_sha256,
    })
}
