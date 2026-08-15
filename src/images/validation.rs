use crate::{
    config::{
        MAX_SLOT,
        MIN_SLOT,
    },
    error::ApiError,
};

/// Validates an image namespace.
///
/// Namespaces are used as directory names and API path components.
///
/// Allowed characters:
///
/// - `a-z`
/// - `0-9`
/// - `-`
/// - `_`
///
/// Uppercase characters are deliberately rejected so names such as
/// `product`, `Product`, and `PRODUCT` cannot become three different
/// directories.
pub(crate) fn validate_namespace(
    namespace: &str,
) -> Result<(), ApiError> {
    if namespace.is_empty()
        || namespace.len() > 32
    {
        return Err(
            ApiError::BadRequest(
                "Invalid namespace"
                    .to_string(),
            )
        );
    }

    if !namespace
        .chars()
        .all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
    {
        return Err(
            ApiError::BadRequest(
                "namespace may only contain a-z, 0-9, '-' and '_'"
                    .to_string(),
            )
        );
    }

    Ok(())
}

/// Validates a resource identifier.
///
/// Resource IDs become filesystem path components, so path separators
/// and other special characters are rejected.
///
/// Allowed characters:
///
/// - ASCII letters
/// - ASCII numbers
/// - `-`
/// - `_`
pub(crate) fn validate_resource_id(
    resource_id: &str,
) -> Result<(), ApiError> {
    if resource_id.is_empty()
        || resource_id.len() > 100
    {
        return Err(
            ApiError::BadRequest(
                "Invalid resource_id"
                    .to_string(),
            )
        );
    }

    if !resource_id
        .chars()
        .all(|character| {
            character
                .is_ascii_alphanumeric()
                || character == '-'
                || character == '_'
        })
    {
        return Err(
            ApiError::BadRequest(
                "resource_id may only contain letters, numbers, '-' and '_'"
                    .to_string(),
            )
        );
    }

    Ok(())
}

/// Validates the image slot.
///
/// Slots are one-based and limited by the configured image policy.
pub(crate) fn validate_slot(
    slot: u8,
) -> Result<(), ApiError> {
    if !(MIN_SLOT..=MAX_SLOT)
        .contains(&slot)
    {
        return Err(
            ApiError::BadRequest(
                format!(
                    "slot must be between {MIN_SLOT} and {MAX_SLOT}"
                )
            )
        );
    }

    Ok(())
}
