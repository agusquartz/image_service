use crate::{
    config::{MAX_DECODE_ALLOC, MAX_IMAGE_DIMENSION},
    error::ApiError,
};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, imageops::FilterType};

use std::{io::Cursor, sync::Arc};

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
    max_output_bytes: usize,
    output_width: u32,
    output_height: u32,
) -> Result<Vec<u8>, ApiError> {
    let permit = semaphore.acquire_owned().await.map_err(|error| {
        ApiError::Internal(format!("Image processing semaphore closed: {error}"))
    })?;

    tokio::task::spawn_blocking(move || {
        // Keep the permit alive for the complete duration of the
        // blocking image-processing operation.
        let _permit = permit;

        process_image_blocking(
            &incoming.bytes,
            &incoming.mime,
            max_output_bytes,
            output_width,
            output_height,
        )
    })
    .await
    .map_err(|error| ApiError::Internal(format!("Image processing task failed: {error}")))?
}

/// Performs format validation, decoding and WebP compression.
///
/// This function is synchronous because the image library and WebP
/// encoder perform CPU-bound work.
fn process_image_blocking(
    bytes: &[u8],
    declared_mime: &str,
    max_output_bytes: usize,
    output_width: u32,
    output_height: u32,
) -> Result<Vec<u8>, ApiError> {
    let format = image::guess_format(bytes)
        .map_err(|_| ApiError::UnsupportedMediaType("Unknown image format".to_string()))?;

    if !supported_format(format) {
        return Err(ApiError::UnsupportedMediaType(format!(
            "Unsupported image format: {format:?}"
        )));
    }

    if !declared_mime_is_acceptable(declared_mime, format) {
        return Err(ApiError::UnsupportedMediaType(format!(
            "Declared MIME type ({declared_mime}) does not match the detected image format ({format:?})"
        )));
    }

    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);

    // Decode limits provide an additional defense against malformed or
    // intentionally expensive image files.
    let mut limits = Limits::default();

    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);

    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);

    limits.max_alloc = Some(MAX_DECODE_ALLOC);

    reader.limits(limits);

    let mut decoder = reader.into_decoder().map_err(|error| {
        ApiError::UnprocessableEntity(format!("Could not initialize image decoder: {error}"))
    })?;

    let orientation = decoder.orientation().map_err(|error| {
        ApiError::UnprocessableEntity(format!("Could not read image orientation: {error}"))
    })?;

    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| {
        ApiError::UnprocessableEntity(format!("Could not decode image: {error}"))
    })?;

    // Normalize the physical pixel layout before any resize or encoding
    // operation.
    //
    // This prevents images whose display orientation is stored only in
    // metadata from becoming rotated after conversion to WebP.
    image.apply_orientation(orientation);

    compress_webp(image, max_output_bytes, output_width, output_height)
}

/// Returns whether the detected source format is accepted by the
/// service.
fn supported_format(format: ImageFormat) -> bool {
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
fn declared_mime_is_acceptable(declared_mime: &str, format: ImageFormat) -> bool {
    let mime = declared_mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    // Some clients intentionally submit generic binary content.
    //
    // The real format is still determined from the uploaded bytes via
    // image::guess_format(), so accepting this does not make the
    // declared MIME authoritative.
    if mime == "application/octet-stream" {
        return true;
    }

    match format {
        ImageFormat::Jpeg => mime == "image/jpeg" || mime == "image/jpg",

        ImageFormat::Png => mime == "image/png",

        ImageFormat::WebP => mime == "image/webp",

        ImageFormat::Gif => mime == "image/gif",

        ImageFormat::Bmp => mime == "image/bmp" || mime == "image/x-ms-bmp",

        ImageFormat::Tiff => mime == "image/tiff",

        _ => false,
    }
}

/// Center-crops to the configured aspect ratio and resamples to the requested
/// dimensions. It finds the highest WebP quality that fits the byte limit. If
/// even quality 0 is too large, it progressively reduces dimensions, always
/// resizing from the same cropped source pixels.
fn compress_webp(
    image: DynamicImage,
    max_output_bytes: usize,
    output_width: u32,
    output_height: u32,
) -> Result<Vec<u8>, ApiError> {
    let cropped = crop_to_target_ratio(image, output_width, output_height);
    let (mut width, mut height) = (output_width, output_height);

    loop {
        let target = cropped.resize_exact(width, height, FilterType::Lanczos3);
        if let Some(encoded) = encode_webp_with_limit(&target, max_output_bytes, width, height) {
            return Ok(encoded);
        }

        if width == 1 && height == 1 {
            return Err(ApiError::UnprocessableEntity(format!(
                "The configured output byte limit ({max_output_bytes}) is too small for even a 1x1 WebP image"
            )));
        }

        let next_width = ((width as f32 * 0.8).floor() as u32).max(1);
        let next_height = ((height as f32 * 0.8).floor() as u32).max(1);
        if (next_width, next_height) == (width, height) {
            return Err(ApiError::UnprocessableEntity(format!(
                "The configured output byte limit ({max_output_bytes}) is too small for a WebP image"
            )));
        }
        (width, height) = (next_width, next_height);
    }
}

/// Encodes at the highest quality that fits, returning `None` if quality 0 is
/// still too large. All quality candidates use this exact pixel buffer.
fn encode_webp_with_limit(
    image: &DynamicImage,
    max_output_bytes: usize,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let rgba = image.to_rgba8();
    let encoder = Encoder::from_rgba(rgba.as_raw(), width, height);

    let highest_quality = encoder.encode(100.0);
    if highest_quality.len() <= max_output_bytes {
        return Some(highest_quality.to_vec());
    }

    let mut low = 0i32;
    let mut high = 99i32;
    let mut best: Option<Vec<u8>> = None;
    while low <= high {
        let quality = low + (high - low) / 2;
        let encoded = encoder.encode(quality as f32);
        if encoded.len() <= max_output_bytes {
            best = Some(encoded.to_vec());
            low = quality + 1;
        } else {
            high = quality - 1;
        }
    }
    best
}

fn crop_to_target_ratio(
    image: DynamicImage,
    output_width: u32,
    output_height: u32,
) -> DynamicImage {
    let (source_width, source_height) = (image.width(), image.height());
    let source_ratio = source_width as u64 * output_height as u64;
    let target_ratio = source_height as u64 * output_width as u64;

    let (crop_x, crop_y, crop_width, crop_height) = if source_ratio > target_ratio {
        let width = ((source_height as u64 * output_width as u64) / output_height as u64)
            .clamp(1, source_width as u64) as u32;
        ((source_width - width) / 2, 0, width, source_height)
    } else {
        let height = ((source_width as u64 * output_height as u64) / output_width as u64)
            .clamp(1, source_height as u64) as u32;
        (0, (source_height - height) / 2, source_width, height)
    };

    image.crop_imm(crop_x, crop_y, crop_width, crop_height)
}

#[cfg(test)]
fn crop_and_resize(image: DynamicImage, output_width: u32, output_height: u32) -> DynamicImage {
    crop_to_target_ratio(image, output_width, output_height).resize_exact(
        output_width,
        output_height,
        FilterType::Lanczos3,
    )
}

#[cfg(test)]
mod tests {
    use super::{compress_webp, crop_and_resize};

    #[test]
    fn center_crops_sides_to_match_the_target_aspect_ratio() {
        let source = image::RgbImage::from_fn(4, 2, |x, y| image::Rgb([x as u8, y as u8, 0]));
        let output = crop_and_resize(image::DynamicImage::ImageRgb8(source.clone()), 2, 2);
        assert_eq!((output.width(), output.height()), (2, 2));
        assert_eq!(output.to_rgb8().get_pixel(0, 0), source.get_pixel(1, 0));
        assert_eq!(output.to_rgb8().get_pixel(1, 1), source.get_pixel(2, 1));
    }

    #[test]
    fn encodes_at_exact_dimensions_and_within_configured_byte_limit() {
        let image = image::DynamicImage::new_rgb8(2000, 1000);
        let output = compress_webp(image, 1024 * 1024, 1024, 1024)
            .expect("image should fit the configured byte limit");
        let decoded = image::load_from_memory_with_format(&output, image::ImageFormat::WebP)
            .expect("output should be valid WebP");
        assert_eq!((decoded.width(), decoded.height()), (1024, 1024));
        assert!(output.len() <= 1024 * 1024);
    }
}
