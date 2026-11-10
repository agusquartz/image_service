use crate::{error::ApiError, state::AppState};

use super::{
    dtos::{ImageListResponse, ImageSummary, UploadResponse},
    model::{ImageKey, IncomingImage},
    reduction::reduce_to_webp,
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
) -> Result<UploadResponse, ApiError> {
    let webp = reduce_to_webp(state.image_jobs.clone(), incoming).await?;

    state.store.write(key, &webp).await?;

    let relative = relative_image_url(key);

    let url = public_image_url(state, &relative);

    Ok(UploadResponse {
        namespace: key.namespace.clone(),

        resource_id: key.resource_id.clone(),

        slot: key.slot,

        url,

        size_bytes: webp.len(),
    })
}

/// Retrieves a stored image.
///
/// Validation has already occurred when constructing [`ImageKey`].
pub(crate) async fn fetch(state: &AppState, key: &ImageKey) -> Result<Vec<u8>, ApiError> {
    state.store.read(key).await
}

pub(crate) async fn delete(state: &AppState, key: &ImageKey) -> Result<(), ApiError> {
    state.store.delete(key).await
}

pub(crate) async fn stat(state: &AppState, key: &ImageKey) -> Result<u64, ApiError> {
    state.store.stat(key).await
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
        })
        .collect();

    Ok(ImageListResponse {
        namespace,
        resource_id,
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
