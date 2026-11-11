use crate::{error::ApiError, state::AppState};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Cursor,
    time::{Duration, SystemTime},
};

use super::{
    dtos::{ImageInfoResponse, ImageListResponse, ImageSummary, PresignResponse, UploadResponse},
    model::{ImageKey, IncomingImage},
    reduction::reduce_to_webp,
    store::StoredMetadata,
};

pub(crate) struct ImageDelivery {
    pub bytes: Vec<u8>,
    pub etag: String,
    pub last_modified: Option<SystemTime>,
}

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
) -> Result<(UploadResponse, bool), ApiError> {
    let policy = state.processing_policy_for(&key.namespace);
    let webp = reduce_to_webp(
        state.image_jobs.clone(),
        incoming,
        policy.max_output_bytes,
        policy.output_width,
        policy.output_height,
    )
    .await?;

    let was_created = if replace_existing {
        state.store.write(key, &webp, state.max_slots).await?
    } else {
        state.store.create(key, &webp, state.max_slots).await?;
        true
    };

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

/// Retrieves a stored image.
///
/// Validation has already occurred when constructing [`ImageKey`].
pub(crate) async fn fetch(state: &AppState, key: &ImageKey) -> Result<ImageDelivery, ApiError> {
    let metadata = state.store.stat(key).await?;
    let bytes = state.store.read(key).await?;
    let etag = metadata
        .etag
        .map(normalize_etag)
        .unwrap_or_else(|| content_etag(&bytes));

    Ok(ImageDelivery {
        bytes,
        etag,
        last_modified: metadata.last_modified,
    })
}

pub(crate) async fn delete(state: &AppState, key: &ImageKey) -> Result<(), ApiError> {
    state.store.delete(key).await
}

pub(crate) async fn delete_resource(
    state: &AppState,
    namespace: &str,
    resource_id: &str,
) -> Result<(), ApiError> {
    state.store.delete_resource(namespace, resource_id).await
}

pub(crate) async fn stat(state: &AppState, key: &ImageKey) -> Result<StoredMetadata, ApiError> {
    state.store.stat(key).await
}

pub(crate) async fn info(state: &AppState, key: &ImageKey) -> Result<ImageInfoResponse, ApiError> {
    let policy = state.processing_policy_for(&key.namespace);
    let metadata = state.store.stat(key).await?;
    let bytes = state.store.read(key).await?;
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
fn relative_image_url(key: &ImageKey) -> String {
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
fn public_image_url(state: &AppState, relative: &str) -> String {
    match &state.public_base_url {
        Some(base) => {
            format!("{base}{relative}")
        }

        None => relative.to_string(),
    }
}
