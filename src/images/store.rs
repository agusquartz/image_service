use crate::error::ApiError;

use std::{
    io::ErrorKind,
    path::{
        Path,
        PathBuf,
    },
};

use tokio::fs;

use super::model::ImageKey;

/// Filesystem-backed image storage.
///
/// The physical layout is:
///
/// ```text
/// <root>/
///     <namespace>/
///         <resource_id>/
///             01.webp
///             02.webp
///             03.webp
///             ...
/// ```
///
/// Example:
///
/// ```text
/// ~/.local/share/image-service/images/
///     products/
///         845/
///             01.webp
///             02.webp
/// ```
#[derive(Debug, Clone)]
pub(crate) struct ImageStore {
    root: PathBuf,
}

impl ImageStore {
    /// Creates a filesystem image store.
    pub fn new(
        root: PathBuf,
    ) -> Self {
        Self {
            root,
        }
    }

    /// Stores a processed WebP image.
    ///
    /// If the requested slot already exists, its contents are replaced.
    pub async fn write(
        &self,
        key: &ImageKey,
        bytes: &[u8],
    ) -> Result<(), ApiError> {
        let path =
            self.image_path(key);

        let parent =
            path
                .parent()
                .ok_or_else(|| {
                    ApiError::Internal(
                        "Could not determine image parent directory"
                            .to_string(),
                    )
                })?;

        fs::create_dir_all(parent)
            .await
            .map_err(|error| {
                ApiError::Io(
                    error.to_string()
                )
            })?;

        fs::write(
            path,
            bytes,
        )
        .await
        .map_err(|error| {
            ApiError::Io(
                error.to_string()
            )
        })?;

        Ok(())
    }

    /// Reads a previously stored WebP image.
    pub async fn read(
        &self,
        key: &ImageKey,
    ) -> Result<Vec<u8>, ApiError> {
        let path =
            self.image_path(key);

        match fs::read(path).await {
            Ok(bytes) => {
                Ok(bytes)
            }

            Err(error)
                if error.kind()
                    == ErrorKind::NotFound =>
            {
                Err(
                    ApiError::NotFound(
                        "Image not found"
                            .to_string(),
                    )
                )
            }

            Err(error) => {
                Err(
                    ApiError::Io(
                        error.to_string()
                    )
                )
            }
        }
    }

    /// Resolves the physical filesystem path corresponding to an image
    /// identifier.
    fn image_path(
        &self,
        key: &ImageKey,
    ) -> PathBuf {
        physical_image_path(
            &self.root,
            key,
        )
    }
}

/// Builds the physical path for an image.
///
/// Keeping this operation centralized prevents handlers and services
/// from making assumptions about the storage layout.
fn physical_image_path(
    root: &Path,
    key: &ImageKey,
) -> PathBuf {
    root
        .join(&key.namespace)
        .join(&key.resource_id)
        .join(
            format!(
                "{:02}.webp",
                key.slot
            )
        )
}
