use bytes::Bytes;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, imageops::FilterType};
use processor_contract::{
    Capability, ProcessingError, ProcessingOutput, ProcessingRequest, Processor,
};
use std::{io::Cursor, time::Instant};
use webp::Encoder;

pub const DEFAULT_MAX_IMAGE_DIMENSION: u32 = 12_000;
pub const DEFAULT_MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct ImageProcessor;

/// Metadata from fully decoding an image without transforming or writing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageInspection {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
}

/// Detects and fully decodes image bytes without encoding or modifying them.
/// The same dimension and allocation limits used by upload processing apply.
pub fn inspect_image(source: &[u8]) -> Result<ImageInspection, ProcessingError> {
    inspect_image_with_limits(
        source,
        DEFAULT_MAX_IMAGE_DIMENSION,
        DEFAULT_MAX_DECODE_ALLOC,
    )
}

/// Inspects an image using the runtime decoder safety limits.
pub fn inspect_image_with_limits(
    source: &[u8],
    max_image_dimension: u32,
    max_decode_allocation_bytes: u64,
) -> Result<ImageInspection, ProcessingError> {
    let format = image::guess_format(source)
        .map_err(|_| ProcessingError::UnsupportedMediaType("Unknown image format".to_string()))?;
    if !supported_format(format) {
        return Err(ProcessingError::UnsupportedMediaType(format!(
            "Unsupported image format: {format:?}"
        )));
    }

    let animated = if format == ImageFormat::WebP {
        image::codecs::webp::WebPDecoder::new(Cursor::new(source))
            .map_err(|error| {
                ProcessingError::InvalidInput(format!("Could not inspect WebP input: {error}"))
            })?
            .has_animation()
    } else {
        false
    };
    let mut reader = ImageReader::with_format(Cursor::new(source), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_image_dimension);
    limits.max_image_height = Some(max_image_dimension);
    limits.max_alloc = Some(max_decode_allocation_bytes);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|error| {
        ProcessingError::InvalidInput(format!("Could not initialize image decoder: {error}"))
    })?;
    let orientation = decoder.orientation().map_err(|error| {
        ProcessingError::InvalidInput(format!("Could not read image orientation: {error}"))
    })?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| {
        ProcessingError::InvalidInput(format!("Could not decode image: {error}"))
    })?;
    image.apply_orientation(orientation);

    Ok(ImageInspection {
        format: format_name(format).to_string(),
        width: image.width(),
        height: image.height(),
        animated,
    })
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        _ => "unsupported",
    }
}

impl Processor for ImageProcessor {
    fn capability(&self) -> Capability {
        Capability::Image
    }

    fn process(&self, request: ProcessingRequest) -> Result<ProcessingOutput, ProcessingError> {
        let (bytes, width, height) = process_image_blocking(request)?;
        Ok(ProcessingOutput {
            bytes,
            media_type: "image/webp",
            width,
            height,
        })
    }
}

/// Performs format validation, decoding and WebP compression.
///
/// This function is synchronous because the image library and WebP
/// encoder perform CPU-bound work.
fn process_image_blocking(
    request: ProcessingRequest,
) -> Result<(Bytes, u32, u32), ProcessingError> {
    let processing_started = Instant::now();
    let decode_started = Instant::now();
    let queue_wait_us = request.queue_wait_us;
    let format = image::guess_format(&request.source)
        .map_err(|_| ProcessingError::UnsupportedMediaType("Unknown image format".to_string()))?;

    if !supported_format(format) {
        return Err(ProcessingError::UnsupportedMediaType(format!(
            "Unsupported image format: {format:?}"
        )));
    }

    if !declared_mime_is_acceptable(request.declared_media_type.as_deref(), format) {
        return Err(ProcessingError::UnsupportedMediaType(format!(
            "Declared MIME type ({}) does not match the detected image format ({format:?})",
            request.declared_media_type.as_deref().unwrap_or_default()
        )));
    }

    // The generic DynamicImage decoder exposes only the first frame. Do not
    // pass an animated WebP through unchanged; keep the existing behavior of
    // converting its decoded frame to a static output.
    let animated_webp = if format == ImageFormat::WebP {
        image::codecs::webp::WebPDecoder::new(Cursor::new(request.source.as_ref()))
            .map_err(|error| {
                ProcessingError::InvalidInput(format!("Could not inspect WebP input: {error}"))
            })?
            .has_animation()
    } else {
        false
    };

    let mut reader = ImageReader::with_format(Cursor::new(request.source.as_ref()), format);

    // Decode limits provide an additional defense against malformed or
    // intentionally expensive image files.
    let mut limits = Limits::default();

    limits.max_image_width = Some(request.max_decode_dimension);

    limits.max_image_height = Some(request.max_decode_dimension);

    limits.max_alloc = Some(request.max_decode_allocation_bytes);

    reader.limits(limits);

    let mut decoder = reader.into_decoder().map_err(|error| {
        ProcessingError::InvalidInput(format!("Could not initialize image decoder: {error}"))
    })?;

    let orientation = decoder.orientation().map_err(|error| {
        ProcessingError::InvalidInput(format!("Could not read image orientation: {error}"))
    })?;

    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| {
        ProcessingError::InvalidInput(format!("Could not decode image: {error}"))
    })?;

    // Normalize the physical pixel layout before any resize or encoding
    // operation.
    //
    // This prevents images whose display orientation is stored only in
    // metadata from becoming rotated after conversion to WebP.
    image.apply_orientation(orientation);

    let decode_us = decode_started.elapsed().as_micros() as u64;
    let compression_started = Instant::now();
    let dimensions = (image.width(), image.height());
    let already_compliant_webp = format == ImageFormat::WebP
        && !animated_webp
        && orientation == Orientation::NoTransforms
        && dimensions == (request.output_width, request.output_height)
        && request.source.len() <= request.max_output_bytes;
    let result = if already_compliant_webp {
        // The input has already been fully decoded and validated. Reuse its
        // allocation rather than resizing and encoding it again.
        Ok((request.source, dimensions.0, dimensions.1))
    } else {
        compress_webp(
            image,
            request.max_output_bytes,
            request.output_width,
            request.output_height,
        )
        .map(|(bytes, width, height)| (Bytes::from(bytes), width, height))
    };
    let compression_us = compression_started.elapsed().as_micros() as u64;

    if tracing::enabled!(target: "image_processing_profile", tracing::Level::DEBUG) {
        tracing::debug!(
            target: "image_processing_profile",
            queue_wait_us,
            decode_us,
            compression_us,
            processing_us = processing_started.elapsed().as_micros() as u64,
            "image processing profile"
        );
    }

    result
}

/// Returns whether the detected source format is accepted by the
/// service.
pub fn supported_format(format: ImageFormat) -> bool {
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
pub fn declared_mime_is_acceptable(declared_mime: Option<&str>, format: ImageFormat) -> bool {
    let Some(declared_mime) = declared_mime else {
        return true;
    };
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
) -> Result<(Vec<u8>, u32, u32), ProcessingError> {
    let cropped = crop_to_target_ratio(image, output_width, output_height);
    let (mut width, mut height) = (output_width, output_height);

    loop {
        let resized = (cropped.width() != width || cropped.height() != height)
            .then(|| cropped.resize_exact(width, height, FilterType::Lanczos3));
        let target = resized.as_ref().unwrap_or(&cropped);
        if let Some(encoded) = encode_webp_with_limit(target, max_output_bytes, width, height) {
            return Ok((encoded, width, height));
        }

        if width == 1 && height == 1 {
            return Err(ProcessingError::InvalidInput(format!(
                "The configured output byte limit ({max_output_bytes}) is too small for even a 1x1 WebP image"
            )));
        }

        let next_width = ((width as f32 * 0.8).floor() as u32).max(1);
        let next_height = ((height as f32 * 0.8).floor() as u32).max(1);
        if (next_width, next_height) == (width, height) {
            return Err(ProcessingError::InvalidInput(format!(
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

    if source_ratio == target_ratio {
        return image;
    }

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
    use super::{
        DEFAULT_MAX_DECODE_ALLOC, DEFAULT_MAX_IMAGE_DIMENSION, ImageProcessor, compress_webp,
        crop_and_resize, declared_mime_is_acceptable, inspect_image,
    };
    use bytes::Bytes;
    use processor_contract::{ProcessingRequest, Processor};
    use std::io::Cursor;

    fn request(
        source: Bytes,
        max_output_bytes: usize,
        output_width: u32,
        output_height: u32,
    ) -> ProcessingRequest {
        ProcessingRequest {
            source,
            declared_media_type: Some("image/webp".to_string()),
            max_output_bytes,
            output_width,
            output_height,
            max_decode_dimension: DEFAULT_MAX_IMAGE_DIMENSION,
            max_decode_allocation_bytes: DEFAULT_MAX_DECODE_ALLOC,
            queue_wait_us: 0,
        }
    }

    fn webp(width: u32, height: u32) -> Vec<u8> {
        compress_webp(
            image::DynamicImage::new_rgb8(width, height),
            1024 * 1024,
            width,
            height,
        )
        .expect("source WebP should encode")
        .0
    }

    #[test]
    fn absent_content_type_uses_detected_image_format() {
        assert!(declared_mime_is_acceptable(None, image::ImageFormat::Png));
        assert!(declared_mime_is_acceptable(
            Some("image/png"),
            image::ImageFormat::Png
        ));
        assert!(!declared_mime_is_acceptable(
            Some("image/jpeg"),
            image::ImageFormat::Png
        ));
    }

    #[test]
    fn inspection_decodes_webp_without_reencoding_and_reports_dimensions() {
        let source = webp(32, 24);
        let original = source.clone();
        let inspected = inspect_image(&source).expect("valid WebP should inspect");
        assert_eq!(inspected.format, "webp");
        assert_eq!((inspected.width, inspected.height), (32, 24));
        assert!(!inspected.animated);
        assert_eq!(source, original);
    }

    #[test]
    fn inspection_reports_other_valid_formats_for_policy_audits() {
        let mut source = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 6)
            .write_to(&mut source, image::ImageFormat::Png)
            .expect("PNG should encode");
        let inspected = inspect_image(source.get_ref()).expect("valid PNG should inspect");
        assert_eq!(inspected.format, "png");
        assert_eq!((inspected.width, inspected.height), (8, 6));
    }

    #[test]
    fn inspection_rejects_invalid_or_truncated_image_data() {
        assert!(inspect_image(b"not an image").is_err());
    }

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
        let (output, width, height) = compress_webp(image, 1024 * 1024, 1024, 1024)
            .expect("image should fit the configured byte limit");
        let decoded = image::load_from_memory_with_format(&output, image::ImageFormat::WebP)
            .expect("output should be valid WebP");
        assert_eq!((decoded.width(), decoded.height()), (width, height));
        assert_eq!((width, height), (1024, 1024));
        assert!(output.len() <= 1024 * 1024);
    }

    #[test]
    fn preserves_compliant_webp_bytes_without_reencoding_or_copying() {
        let source = Bytes::from(webp(32, 32));
        let source_ptr = source.as_ptr();
        let output = ImageProcessor
            .process(request(source.clone(), source.len(), 32, 32))
            .expect("compliant WebP should be accepted");

        assert_eq!(output.bytes, source);
        assert_eq!(output.bytes.as_ptr(), source_ptr);
        assert_eq!((output.width, output.height), (32, 32));
    }

    #[test]
    fn resizes_webp_when_dimensions_do_not_match_the_configured_target() {
        let source = webp(32, 16);
        let output = ImageProcessor
            .process(request(Bytes::from(source.clone()), 1024 * 1024, 16, 16))
            .expect("valid WebP should be resized");

        assert_ne!(output.bytes.as_ref(), source.as_slice());
        assert_eq!((output.width, output.height), (16, 16));
    }

    #[test]
    fn recompresses_webp_when_it_exceeds_the_output_byte_limit() {
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([(x * 37) as u8, (y * 53) as u8, (x * y * 17) as u8])
        }));
        let source = compress_webp(source, 1024 * 1024, 64, 64)
            .expect("source WebP should fit a generous limit")
            .0;
        let output = ImageProcessor
            .process(request(Bytes::from(source), 64, 64, 64))
            .expect("processor should reduce the output to the configured byte limit");

        assert!(output.bytes.len() <= 64);
        assert!(output.width <= 64 && output.height <= 64);
    }
}
