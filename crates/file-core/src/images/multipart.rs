use crate::error::ApiError;

use axum::extract::Multipart;
use opendal::Writer;
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use super::model::IncomingImage;
use super::store::ImageStore;

/// A fully received source that stayed in RAM or spilled to private staging.
pub(crate) enum ReceivedImage {
    Buffered(IncomingImage),
    Staged {
        staging_id: String,
        source_bytes: usize,
        mime: Option<String>,
        _cleanup: TransferStagingCleanup,
    },
}

/// Deletes partial or completed transfer staging if request handling fails or
/// is cancelled before the staged source is promoted into processing memory.
pub(crate) struct TransferStagingCleanup {
    store: ImageStore,
    staging_id: String,
    writer: Option<Writer>,
    armed: bool,
}

impl TransferStagingCleanup {
    fn new(store: ImageStore, staging_id: String, writer: Writer) -> Self {
        Self {
            store,
            staging_id,
            writer: Some(writer),
            armed: true,
        }
    }

    #[cfg(test)]
    fn completed(store: ImageStore, staging_id: String) -> Self {
        Self {
            store,
            staging_id,
            writer: None,
            armed: true,
        }
    }

    async fn remove_now(&mut self) -> Result<(), ApiError> {
        self.store.delete_transfer_staged(&self.staging_id).await?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for TransferStagingCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let store = self.store.clone();
        let staging_id = self.staging_id.clone();
        let mut writer = self.writer.take();
        if let Ok(runtime) = Handle::try_current() {
            runtime.spawn(async move {
                if let Some(writer) = writer.as_mut()
                    && let Err(error) = writer.abort().await
                {
                    tracing::warn!(error = %error, "failed to abort incomplete upload staging write");
                }
                if let Err(error) = store.delete_transfer_staged(&staging_id).await {
                    tracing::warn!(error = ?error, "failed to remove temporary upload staging object");
                }
            });
        }
    }
}

impl ReceivedImage {
    pub(crate) fn staged_size(&self) -> Option<usize> {
        match self {
            Self::Buffered(_) => None,
            Self::Staged { source_bytes, .. } => Some(*source_bytes),
        }
    }

    pub(crate) async fn into_incoming(self, store: &ImageStore) -> Result<IncomingImage, ApiError> {
        match self {
            Self::Buffered(image) => Ok(image),
            Self::Staged {
                staging_id,
                source_bytes,
                mime,
                mut _cleanup,
            } => {
                let bytes = store.read_transfer_staged(&staging_id).await?;
                if bytes.len() != source_bytes {
                    return Err(ApiError::Storage(
                        "Staged upload size differs from received byte count".into(),
                    ));
                }
                _cleanup.remove_now().await?;
                Ok(IncomingImage { bytes, mime })
            }
        }
    }
}

/// Receives one multipart image incrementally. The configured threshold only
/// controls where bytes are held; `max_source_bytes` remains the hard limit.
pub(crate) async fn receive_multipart_image(
    mut multipart: Multipart,
    max_source_bytes: usize,
    memory_threshold_bytes: usize,
    store: &ImageStore,
) -> Result<ReceivedImage, ApiError> {
    let mut seen_image = false;
    let mut source_bytes = 0_usize;
    let mut bytes = Vec::new();
    let mut staging_id: Option<String> = None;
    let mut cleanup: Option<TransferStagingCleanup> = None;
    let mut type_prefix = Vec::with_capacity(32);
    let mut format_checked = false;
    let mut mime = None;

    while let Some(mut field) = multipart.next_field().await.map_err(map_multipart_error)? {
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

        while let Some(chunk) = field.chunk().await.map_err(map_image_chunk_error)? {
            if chunk.len() > max_source_bytes.saturating_sub(source_bytes) {
                return Err(payload_too_large(max_source_bytes));
            }
            validate_chunk_type(
                &chunk,
                &mut type_prefix,
                &mut format_checked,
                mime.as_deref(),
            )?;

            if cleanup.is_none()
                && crosses_memory_threshold(source_bytes, chunk.len(), memory_threshold_bytes)
            {
                let id = uuid::Uuid::new_v4().to_string();
                let staged_writer = store.open_transfer_writer(&id).await?;
                cleanup = Some(TransferStagingCleanup::new(
                    store.clone(),
                    id.clone(),
                    staged_writer,
                ));
                if !bytes.is_empty() {
                    let prefix = std::mem::take(&mut bytes);
                    cleanup
                        .as_mut()
                        .and_then(|cleanup| cleanup.writer.as_mut())
                        .expect("staged writer is present until close")
                        .write(axum::body::Bytes::from(prefix))
                        .await
                        .map_err(|error| ApiError::Storage(error.to_string()))?;
                }
                staging_id = Some(id);
            }

            if let Some(writer) = cleanup.as_mut().and_then(|cleanup| cleanup.writer.as_mut()) {
                writer
                    .write(chunk.clone())
                    .await
                    .map_err(|error| ApiError::Storage(error.to_string()))?;
            } else {
                bytes.extend_from_slice(&chunk);
            }
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

    if let Some(cleanup_guard) = cleanup.as_mut() {
        let mut writer = cleanup_guard
            .writer
            .take()
            .expect("staged writer is present until close");
        if let Err(error) = writer.close().await {
            cleanup_guard.writer = Some(writer);
            return Err(ApiError::Storage(error.to_string()));
        }
        Ok(ReceivedImage::Staged {
            staging_id: staging_id.expect("writer always has a staging id"),
            source_bytes,
            mime,
            _cleanup: cleanup.take().expect("writer always has a cleanup guard"),
        })
    } else {
        Ok(ReceivedImage::Buffered(IncomingImage {
            bytes: bytes.into(),
            mime,
        }))
    }
}

fn map_multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::PayloadTooLarge(error.body_text())
    } else {
        ApiError::BadRequest(format!("Invalid multipart request: {error}"))
    }
}

fn map_image_chunk_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::PayloadTooLarge(error.body_text())
    } else {
        ApiError::BadRequest(format!("Could not read image data: {error}"))
    }
}

fn payload_too_large(max_source_bytes: usize) -> ApiError {
    ApiError::PayloadTooLarge(format!(
        "The source image exceeds the {} MB limit",
        max_source_bytes.div_ceil(1024 * 1024)
    ))
}

fn crosses_memory_threshold(
    received_bytes: usize,
    next_chunk_bytes: usize,
    threshold: usize,
) -> bool {
    received_bytes.saturating_add(next_chunk_bytes) > threshold
}

fn validate_chunk_type(
    chunk: &[u8],
    type_prefix: &mut Vec<u8>,
    format_checked: &mut bool,
    mime: Option<&str>,
) -> Result<(), ApiError> {
    if !*format_checked && type_prefix.len() < 32 {
        let prefix_len = (32 - type_prefix.len()).min(chunk.len());
        type_prefix.extend_from_slice(&chunk[..prefix_len]);
        if let Ok(format) = image::guess_format(type_prefix) {
            if !super::reduction::supported_format(format)
                || !super::reduction::declared_mime_is_acceptable(mime, format)
            {
                return Err(ApiError::UnsupportedMediaType(format!(
                    "Declared Content-Type does not match a supported image format ({format:?})"
                )));
            }
            *format_checked = true;
        }
    }
    Ok(())
}

pub(crate) struct StagedImageInfo {
    pub source_bytes: usize,
    pub mime: Option<String>,
    pub content_sha256: String,
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

#[cfg(test)]
mod tests {
    use super::{ReceivedImage, TransferStagingCleanup, crosses_memory_threshold};
    use crate::images::ImageStore;
    use opendal::Operator;
    use std::collections::HashMap;

    #[test]
    fn staging_starts_only_after_observed_bytes_cross_the_threshold() {
        assert!(!crosses_memory_threshold(0, 8, 8));
        assert!(crosses_memory_threshold(8, 1, 8));
        assert!(!crosses_memory_threshold(usize::MAX, 1, usize::MAX));
    }

    #[tokio::test]
    async fn promoted_source_is_removed_from_transfer_staging() {
        let temp_dir = tempfile::tempdir().unwrap();
        let options = HashMap::from([(
            "root".to_string(),
            temp_dir.path().to_string_lossy().into_owned(),
        )]);
        let store = ImageStore::new(Operator::via_iter("fs", options).unwrap());
        let staging_id = uuid::Uuid::new_v4().to_string();
        let source = axum::body::Bytes::from_static(b"staged source bytes");
        let mut writer = store.open_transfer_writer(&staging_id).await.unwrap();
        writer.write(source.clone()).await.unwrap();
        writer.close().await.unwrap();

        let received = ReceivedImage::Staged {
            staging_id: staging_id.clone(),
            source_bytes: source.len(),
            mime: None,
            _cleanup: TransferStagingCleanup::completed(store.clone(), staging_id.clone()),
        };
        let promoted = received.into_incoming(&store).await.unwrap();

        assert_eq!(promoted.bytes, source);
        assert!(store.read_transfer_staged(&staging_id).await.is_err());
    }
}
