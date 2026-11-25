use crate::{
    config::ProcessingPolicy,
    error::ApiError,
    state::{AppState, UploadAdmission},
};
use axum::body::Bytes;
use processor_contract::{ProcessingError, ProcessingRequest, Processor};
use std::{sync::Arc, time::Instant};
use tokio::sync::Semaphore;

pub(crate) fn supported_format(format: image::ImageFormat) -> bool {
    processor_image::supported_format(format)
}

pub(crate) fn declared_mime_is_acceptable(
    declared_mime: Option<&str>,
    format: image::ImageFormat,
) -> bool {
    processor_image::declared_mime_is_acceptable(declared_mime, format)
}

/// Runs the image processor behind the Core's bounded blocking-work gate.
///
/// The processor crate owns image decoding and encoding. The Core owns
/// admission, queueing, and the Tokio blocking-pool boundary.
pub(crate) async fn reduce_to_webp(
    state: &AppState,
    processor: Arc<dyn Processor>,
    semaphore: Arc<Semaphore>,
    incoming: super::model::IncomingImage,
    admission: UploadAdmission,
    policy: &ProcessingPolicy,
) -> Result<Bytes, ApiError> {
    let queue_started = Instant::now();
    let memory_permit = state
        .admit_image_processing(policy.output_width, policy.output_height)
        .await?;
    let permit = semaphore.acquire_owned().await.map_err(|error| {
        ApiError::Internal(format!("Image processing semaphore closed: {error}"))
    })?;
    let queue_wait_us = queue_started.elapsed().as_micros() as u64;
    let request = ProcessingRequest {
        source: incoming.bytes,
        declared_media_type: incoming.mime,
        max_output_bytes: policy.max_output_bytes,
        output_width: policy.output_width,
        output_height: policy.output_height,
        max_decode_dimension: state.max_decode_dimension,
        max_decode_allocation_bytes: state.max_decode_allocation_bytes,
        queue_wait_us,
    };

    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _memory_permit = memory_permit;
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
