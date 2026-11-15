use crate::{error::ApiError, state::UploadAdmission};
use processor_contract::{ProcessingError, ProcessingRequest, Processor};
use std::{sync::Arc, time::Instant};
use tokio::sync::Semaphore;

pub(crate) fn supported_format(format: image::ImageFormat) -> bool {
    image_processor_image::supported_format(format)
}

pub(crate) fn declared_mime_is_acceptable(
    declared_mime: Option<&str>,
    format: image::ImageFormat,
) -> bool {
    image_processor_image::declared_mime_is_acceptable(declared_mime, format)
}

/// Runs the image processor behind the Core's bounded blocking-work gate.
///
/// The processor crate owns image decoding and encoding. The Core owns
/// admission, queueing, and the Tokio blocking-pool boundary.
pub(crate) async fn reduce_to_webp(
    processor: Arc<dyn Processor>,
    semaphore: Arc<Semaphore>,
    incoming: super::model::IncomingImage,
    admission: UploadAdmission,
    max_output_bytes: usize,
    output_width: u32,
    output_height: u32,
) -> Result<Vec<u8>, ApiError> {
    let queue_started = Instant::now();
    let permit = semaphore.acquire_owned().await.map_err(|error| {
        ApiError::Internal(format!("Image processing semaphore closed: {error}"))
    })?;
    let queue_wait_us = queue_started.elapsed().as_micros() as u64;
    let request = ProcessingRequest {
        source: incoming.bytes,
        declared_media_type: incoming.mime,
        max_output_bytes,
        output_width,
        output_height,
        queue_wait_us,
    };

    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _admission = admission;
        processor.process(request)
    })
    .await
    .map_err(|error| ApiError::Internal(format!("Image processing task failed: {error}")))?
    .map(|output| output.bytes)
    .map_err(map_processing_error)
}

fn map_processing_error(error: ProcessingError) -> ApiError {
    match error {
        ProcessingError::UnsupportedMediaType(message) => ApiError::UnsupportedMediaType(message),
        ProcessingError::InvalidInput(message) => ApiError::UnprocessableEntity(message),
        ProcessingError::Failed(message) => ApiError::Internal(message),
    }
}
