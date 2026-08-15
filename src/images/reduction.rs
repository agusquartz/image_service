use crate::{
    config::{
        MAX_DECODE_ALLOC,
        MAX_IMAGE_DIMENSION,
        MAX_OUTPUT_BYTES,
        MAX_RESIZE_ROUNDS,
        MIN_LONGEST_SIDE,
        RESIZE_SCALE,
        WEBP_QUALITIES,
    },
    error::ApiError,
};

use image::{
    imageops::FilterType,
    DynamicImage,
    ImageFormat,
    ImageReader,
    Limits,
};

use std::{
    io::Cursor,
    sync::Arc,
};

use tokio::sync::Semaphore;

use webp::Encoder;

use super::model::IncomingImage;

/// Converts an incoming image into a WebP image that satisfies the
/// configured output size limit.
///
/// Image decoding and compression are CPU-intensive operations.
/// Therefore:
///
/// 1. A semaphore limits the number of concurrent jobs.
/// 2. Processing is moved to Tokio's blocking thread pool.
/// 3. The async runtime remains available for HTTP and filesystem I/O.
pub(crate) async fn reduce_to_webp(
    semaphore: Arc<Semaphore>,
    incoming: IncomingImage,
) -> Result<Vec<u8>, ApiError> {
    let permit =
        semaphore
            .acquire_owned()
            .await
            .map_err(|error| {
                ApiError::Internal(
                    format!(
                        "Image processing semaphore closed: {error}"
                    )
                )
            })?;

    tokio::task::spawn_blocking(
        move || {
            // Keep the permit alive for the complete duration of the
            // blocking image-processing operation.
            let _permit = permit;

            process_image_blocking(
                &incoming.bytes,
                &incoming.mime,
            )
        },
    )
    .await
    .map_err(|error| {
        ApiError::Internal(
            format!(
                "Image processing task failed: {error}"
            )
        )
    })?
}

/// Performs format validation, decoding and WebP compression.
///
/// This function is synchronous because the image library and WebP
/// encoder perform CPU-bound work.
fn process_image_blocking(
    bytes: &[u8],
    declared_mime: &str,
) -> Result<Vec<u8>, ApiError> {
    let format =
        image::guess_format(bytes)
            .map_err(|_| {
                ApiError::UnsupportedMediaType(
                    "Unknown image format"
                        .to_string(),
                )
            })?;

    if !supported_format(format) {
        return Err(
            ApiError::UnsupportedMediaType(
                format!(
                    "Unsupported image format: {format:?}"
                )
            )
        );
    }

    if !mime_matches_format(
        declared_mime,
        format,
    ) {
        return Err(
            ApiError::UnsupportedMediaType(
                format!(
                    "Declared MIME type ({declared_mime}) does not match the detected image format ({format:?})"
                )
            )
        );
    }

    let mut reader =
        ImageReader::with_format(
            Cursor::new(bytes),
            format,
        );

    // Decode limits provide an additional defense against malformed or
    // intentionally expensive image files.
    let mut limits =
        Limits::default();

    limits.max_image_width =
        Some(MAX_IMAGE_DIMENSION);

    limits.max_image_height =
        Some(MAX_IMAGE_DIMENSION);

    limits.max_alloc =
        Some(MAX_DECODE_ALLOC);

    reader.limits(limits);

    let image =
        reader
            .decode()
            .map_err(|error| {
                ApiError::UnprocessableEntity(
                    format!(
                        "Could not decode image: {error}"
                    )
                )
            })?;

    compress_webp(image)
}

/// Returns whether the detected source format is accepted by the
/// service.
fn supported_format(
    format: ImageFormat,
) -> bool {
    matches!(
        format,
        ImageFormat::Jpeg
            | ImageFormat::Png
            | ImageFormat::WebP
            | ImageFormat::Gif
            | ImageFormat::Bmp
            | ImageFormat::Tiff
    )
}

/// Verifies that the MIME type declared by the multipart field matches
/// the actual format detected from the image bytes.
///
/// This prevents clients from disguising arbitrary formats by merely
/// changing the Content-Type header.
fn mime_matches_format(
    declared_mime: &str,
    format: ImageFormat,
) -> bool {
    let mime =
        declared_mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

    match format {
        ImageFormat::Jpeg => {
            mime == "image/jpeg"
                || mime == "image/jpg"
        }

        ImageFormat::Png => {
            mime == "image/png"
        }

        ImageFormat::WebP => {
            mime == "image/webp"
        }

        ImageFormat::Gif => {
            mime == "image/gif"
        }

        ImageFormat::Bmp => {
            mime == "image/bmp"
                || mime == "image/x-ms-bmp"
        }

        ImageFormat::Tiff => {
            mime == "image/tiff"
        }

        _ => false,
    }
}

/// Encodes an image as WebP while attempting to keep the output under
/// [`MAX_OUTPUT_BYTES`].
///
/// The reduction strategy has two phases.
///
/// ## Phase 1: quality reduction
///
/// For the current dimensions, several WebP quality levels are tested,
/// from highest to lowest.
///
/// The first result satisfying the output limit is returned.
///
/// ## Phase 2: dimension reduction
///
/// If none of the configured quality levels fits, width and height are
/// reduced by [`RESIZE_SCALE`] and the quality sequence is attempted
/// again.
///
/// This continues until one of the following happens:
///
/// - The output fits.
/// - The maximum number of resize rounds is reached.
/// - The longest side reaches [`MIN_LONGEST_SIDE`].
fn compress_webp(
    mut image: DynamicImage,
) -> Result<Vec<u8>, ApiError> {
    for resize_round
        in 0..=MAX_RESIZE_ROUNDS
    {
        let width =
            image.width();

        let height =
            image.height();

        let rgba =
            image.to_rgba8();

        let encoder =
            Encoder::from_rgba(
                rgba.as_raw(),
                width,
                height,
            );

        // First try progressively lower WebP quality levels without
        // changing image dimensions.
        for quality
            in WEBP_QUALITIES
        {
            let encoded =
                encoder.encode(quality);

            if encoded.len()
                <= MAX_OUTPUT_BYTES
            {
                return Ok(
                    encoded.to_vec()
                );
            }
        }

        // All quality levels failed. Decide whether another resize is
        // allowed.
        if resize_round
            >= MAX_RESIZE_ROUNDS
        {
            break;
        }

        if width.max(height)
            <= MIN_LONGEST_SIDE
        {
            break;
        }

        let new_width =
            (
                width as f32
                    * RESIZE_SCALE
            )
                .round()
                .max(1.0)
                as u32;

        let new_height =
            (
                height as f32
                    * RESIZE_SCALE
            )
                .round()
                .max(1.0)
                as u32;

        // Defensive check against a scale operation that would no
        // longer modify the dimensions.
        if new_width == width
            && new_height == height
        {
            break;
        }

        image =
            image.resize_exact(
                new_width,
                new_height,
                FilterType::Lanczos3,
            );
    }

    Err(
        ApiError::UnprocessableEntity(
            format!(
                "Could not reduce the image below {} KB",
                MAX_OUTPUT_BYTES / 1024
            )
        )
    )
}
