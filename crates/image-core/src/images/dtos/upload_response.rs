use serde::Serialize;

/// Response returned after successfully storing an image.
#[derive(Debug, Serialize)]
pub(crate) struct UploadResponse {
    /// Logical resource namespace.
    pub namespace: String,

    /// Identifier of the resource owning the image.
    pub resource_id: String,

    /// Image position within the resource.
    pub slot: u8,

    /// URL that can be used to retrieve the image.
    pub url: String,

    /// Final encoded WebP size.
    pub size_bytes: usize,
}
