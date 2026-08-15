use crate::error::ApiError;

use super::validation::{
    validate_namespace,
    validate_resource_id,
    validate_slot,
};

/// Identifies one image belonging to an application resource.
///
/// For example:
///
/// - namespace: `products`
/// - resource_id: `47`
/// - slot: `2`
///
/// Physical representation:
///
/// `products/47/02.webp`
#[derive(Debug, Clone)]
pub(crate) struct ImageKey {
    pub namespace: String,
    pub resource_id: String,
    pub slot: u8,
}

impl ImageKey {
    /// Creates and validates an image identifier.
    ///
    /// Validation at construction time prevents invalid identifiers
    /// from reaching storage or image services.
    pub fn new(
        namespace: String,
        resource_id: String,
        slot: u8,
    ) -> Result<Self, ApiError> {
        validate_namespace(
            &namespace
        )?;

        validate_resource_id(
            &resource_id
        )?;

        validate_slot(slot)?;

        Ok(Self {
            namespace,
            resource_id,
            slot,
        })
    }
}

/// Raw image received from a multipart request.
///
/// The declared MIME type is preserved so the processing layer can
/// verify that it matches the actual image format detected from the
/// file bytes.
pub(crate) struct IncomingImage {
    pub bytes: Vec<u8>,
    pub mime: String,
}
