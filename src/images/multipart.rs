use crate::error::ApiError;

use axum::extract::Multipart;

use super::model::IncomingImage;

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
