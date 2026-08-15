use crate::{
    config::MAX_SOURCE_BYTES,
    error::ApiError,
};

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
/// - The image has no declared Content-Type.
/// - The image is empty.
/// - The image exceeds the configured source size limit.
pub(crate) async fn read_multipart_image(
    mut multipart: Multipart,
) -> Result<IncomingImage, ApiError> {
    let mut image:
        Option<IncomingImage> = None;

    while let Some(field) =
        multipart
            .next_field()
            .await
            .map_err(|error| {
                ApiError::BadRequest(
                    format!(
                        "Invalid multipart request: {error}"
                    )
                )
            })?
    {
        let name =
            field
                .name()
                .unwrap_or("")
                .to_string();

        if name != "image" {
            continue;
        }

        if image.is_some() {
            return Err(
                ApiError::BadRequest(
                    "Only one 'image' field is allowed"
                        .to_string(),
                )
            );
        }

        let mime =
            field
                .content_type()
                .map(str::to_string)
                .ok_or_else(|| {
                    ApiError::UnsupportedMediaType(
                        "The image field must include a Content-Type"
                            .to_string(),
                    )
                })?;

        let bytes =
            field
                .bytes()
                .await
                .map_err(|error| {
                    ApiError::BadRequest(
                        format!(
                            "Could not read image data: {error}"
                        )
                    )
                })?;

        if bytes.is_empty() {
            return Err(
                ApiError::BadRequest(
                    "The uploaded image is empty"
                        .to_string(),
                )
            );
        }

        if bytes.len()
            > MAX_SOURCE_BYTES
        {
            return Err(
                ApiError::PayloadTooLarge(
                    format!(
                        "The source image exceeds the {} MB limit",
                        MAX_SOURCE_BYTES
                            / 1024
                            / 1024
                    )
                )
            );
        }

        image =
            Some(
                IncomingImage {
                    bytes:
                        bytes.to_vec(),

                    mime,
                }
            );
    }

    image.ok_or_else(|| {
        ApiError::BadRequest(
            "Missing multipart field 'image'"
                .to_string(),
        )
    })
}
