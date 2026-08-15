use crate::{error::ApiError, state::AppState};

use super::{
    dtos::UploadResponse,
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
