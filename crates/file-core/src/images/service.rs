use crate::{
    error::ApiError,
    state::{AppState, UploadAdmission},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Cursor, time::Duration};

use super::{
    dtos::{ImageInfoResponse, ImageListResponse, ImageSummary, PresignResponse, UploadResponse},
    model::{ImageKey, IncomingImage},
    reduction::reduce_to_webp,
    store::{ImageOperationToken, StoredMetadata},
};

/// Processes and stores an uploaded image.
///
/// This is the application-level use case.
///
/// It does not know anything about multipart extraction or Axum HTTP
/// responses. Those responsibilities belong to the handler layer.
pub(crate) async fn upload(
    state: &AppState,
    key: &ImageKey,
    incoming: IncomingImage,
    replace_existing: bool,
    admission: UploadAdmission,
    operation: ImageOperationToken,
) -> Result<(UploadResponse, bool), ApiError> {
    let policy = state.processing_policy_for(&key.namespace);
    let webp = reduce_to_webp(
        state,
        state.image_processor.clone(),
        state.image_jobs.clone(),
        incoming,
        admission,
        &policy,
    )
    .await?;
    let target_lock = state.async_resource_lock(&key.namespace, &key.resource_id);
    let _target_guard = target_lock.lock().await;
    crate::image_jobs::cancel_target_for_sync_mutation(state, key).await?;
    let mutation_id = crate::storage_usage::begin_http_mutation(
        state,
        key,
        if replace_existing { "PUT" } else { "POST" },
    )
    .await?;
    let write_result = if replace_existing {
        state
            .store
            .write_with_token(key, webp.clone(), state.max_slots, &operation)
            .await
    } else {
        state
            .store
            .create_with_token(key, webp.clone(), state.max_slots, &operation)
            .await
            .map(|metadata| (true, metadata))
    };
    let (was_created, written_metadata) = match write_result {
        Ok(result) => result,
        Err(error) => {
            crate::storage_usage::queue_mutation_recovery(
                state,
                mutation_id,
                if replace_existing { "PUT" } else { "POST" },
                &key.namespace,
                &key.resource_id,
                Some(key.slot),
            )
            .await;
            return Err(error);
        }
    };
    crate::storage_usage::complete_http_mutation(
        state,
        mutation_id,
        &key.namespace,
        &key.resource_id,
        Some(key.slot),
        Some(&policy),
        Some(&written_metadata),
    )
    .await?;

    let relative = relative_image_url(key);

    let url = public_image_url(state, &relative);

    Ok((
        UploadResponse {
            namespace: key.namespace.clone(),

            resource_id: key.resource_id.clone(),

            slot: key.slot,

            url,

            size_bytes: webp.len(),
        },
        was_created,
    ))
}

/// Opens a version-consistent storage reader for streaming the image response.
pub(crate) async fn open_download(
    state: &AppState,
    key: &ImageKey,
) -> Result<(super::store::StoredMetadata, opendal::Reader), ApiError> {
    state
        .store
        .open_image_reader(key, state.download_stream_chunk_bytes())
        .await
}

pub(crate) async fn delete(state: &AppState, key: &ImageKey) -> Result<(), ApiError> {
    let target_lock = state.async_resource_lock(&key.namespace, &key.resource_id);
    let _target_guard = target_lock.lock().await;
    crate::image_jobs::cancel_target_for_sync_mutation(state, key).await?;
    let mutation_id = crate::storage_usage::begin_http_mutation(state, key, "DELETE").await?;
    if let Err(error) = state.store.delete(key).await {
        crate::storage_usage::queue_mutation_recovery(
            state,
            mutation_id,
            "DELETE",
            &key.namespace,
            &key.resource_id,
            Some(key.slot),
        )
        .await;
        return Err(error);
    }
    crate::storage_usage::complete_http_mutation(
        state,
        mutation_id,
        &key.namespace,
        &key.resource_id,
        Some(key.slot),
        None,
        None,
    )
    .await
}

pub(crate) async fn delete_resource(
    state: &AppState,
    namespace: &str,
    resource_id: &str,
) -> Result<(), ApiError> {
    let target_lock = state.async_resource_lock(namespace, resource_id);
    let _target_guard = target_lock.lock().await;
    crate::image_jobs::cancel_resource_for_sync_delete(state, namespace, resource_id).await?;
    let mutation_id =
        crate::storage_usage::begin_resource_delete(state, namespace, resource_id).await?;
    if let Err(error) = state.store.delete_resource(namespace, resource_id).await {
        crate::storage_usage::queue_mutation_recovery(
            state,
            mutation_id,
            "DELETE",
            namespace,
            resource_id,
            None,
        )
        .await;
        return Err(error);
    }
    crate::storage_usage::complete_http_mutation(
        state,
        mutation_id,
        namespace,
        resource_id,
        None,
        None,
        None,
    )
    .await
}

pub(crate) async fn stat(state: &AppState, key: &ImageKey) -> Result<StoredMetadata, ApiError> {
    state.store.stat(key).await
}

pub(crate) async fn is_current_snapshot(
    state: &AppState,
    key: &ImageKey,
    metadata: &StoredMetadata,
) -> Result<bool, ApiError> {
    state.store.is_current_snapshot(key, metadata).await
}

pub(crate) async fn info(state: &AppState, key: &ImageKey) -> Result<ImageInfoResponse, ApiError> {
    let policy = state.processing_policy_for(&key.namespace);
    let (metadata, bytes) = state.store.read_snapshot(key).await?;
    let dimensions = image::ImageReader::with_format(Cursor::new(&bytes), image::ImageFormat::WebP)
        .into_dimensions()
        .map_err(|error| {
            ApiError::Internal(format!("Could not read stored WebP dimensions: {error}"))
        })?;

    Ok(ImageInfoResponse {
        namespace: key.namespace.clone(),
        resource_id: key.resource_id.clone(),
        slot: key.slot,
        size_bytes: metadata.content_length,
        format: "webp",
        width: dimensions.0,
        height: dimensions.1,
        profile: policy.profile_name,
        etag: Some(
            metadata
                .etag
                .map(normalize_etag)
                .unwrap_or_else(|| content_etag(&bytes)),
        ),
        last_modified: metadata.last_modified.map(httpdate::fmt_http_date),
    })
}

pub(crate) async fn presign_read(
    state: &AppState,
    key: &ImageKey,
) -> Result<PresignResponse, ApiError> {
    state.store.stat(key).await?;
    let expires_in = state.presign_ttl_seconds;
    let request = state
        .store
        .presign_read(key, Duration::from_secs(expires_in))
        .await?;
    let headers = request
        .header()
        .iter()
        .filter_map(|(name, value)| {
            if name == axum::http::header::HOST {
                return None;
            }
            Some((name.to_string(), value.to_str().ok()?.to_string()))
        })
        .collect::<BTreeMap<_, _>>();

    Ok(PresignResponse {
        method: request.method().to_string(),
        url: request.uri().to_string(),
        headers,
        expires_in,
    })
}

pub(crate) fn normalize_etag(etag: String) -> String {
    if etag.starts_with('"') || etag.starts_with("W/\"") {
        etag
    } else {
        format!("\"{etag}\"")
    }
}

fn content_etag(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("\"{hex}\"")
}

pub(crate) async fn list_resource(
    state: &AppState,
    namespace: String,
    resource_id: String,
) -> Result<ImageListResponse, ApiError> {
    let stored = state.store.list_resource(&namespace, &resource_id).await?;
    let images = stored
        .into_iter()
        .map(|image| ImageSummary {
            slot: image.slot,
            url: public_image_url(
                state,
                &relative_image_url_for(&namespace, &resource_id, image.slot),
            ),
            size_bytes: image.content_length,
            etag: image.etag.map(normalize_etag),
            last_modified: image.last_modified.map(httpdate::fmt_http_date),
        })
        .collect();

    Ok(ImageListResponse {
        namespace,
        resource_id,
        max_creatable_slot: state.max_slots,
        images,
    })
}

fn relative_image_url_for(namespace: &str, resource_id: &str, slot: u8) -> String {
    format!("/api/images/{namespace}/{resource_id}/{slot}")
}

/// Builds the API-relative URL for an image.
pub(crate) fn relative_image_url(key: &ImageKey) -> String {
    format!(
        "/api/images/{}/{}/{}",
        key.namespace, key.resource_id, key.slot,
    )
}

/// Converts a relative API URL into a public URL when a public base URL
/// is configured.
///
/// Without `IMAGE_PUBLIC_BASE_URL`:
///
/// ```text
/// /api/images/products/123/1
/// ```
///
/// With:
///
/// ```text
/// IMAGE_PUBLIC_BASE_URL=https://images.example.com
/// ```
///
/// the result becomes:
///
/// ```text
/// https://images.example.com/api/images/products/123/1
/// ```
pub(crate) fn public_image_url(state: &AppState, relative: &str) -> String {
    match &state.public_base_url {
        Some(base) => {
            format!("{base}{relative}")
        }

        None => relative.to_string(),
    }
}
