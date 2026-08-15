use crate::error::ApiError;

use atomic_write_file::AtomicWriteFile;

use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
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
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Stores a processed WebP image.
    ///
    /// The image is written atomically:
    ///
    /// 1. A temporary file is created in the destination directory.
    /// 2. The complete image is written to that temporary file.
    /// 3. The temporary file is committed over the destination.
    ///
    /// Readers therefore observe either the previous complete image or the
    /// new complete image, rather than a partially written file.
    ///
    /// If the requested slot already exists, it is replaced.
    pub async fn write(&self, key: &ImageKey, bytes: &[u8]) -> Result<(), ApiError> {
        let path = self.image_path(key);

        let parent = path.parent().ok_or_else(|| {
            ApiError::Internal("Could not determine image parent directory".to_string())
        })?;

        fs::create_dir_all(parent)
            .await
            .map_err(|error| ApiError::Io(error.to_string()))?;

        // AtomicWriteFile performs synchronous filesystem operations.
        //
        // Keep them away from Tokio's asynchronous worker threads.
        //
        // The processed image is already bounded by MAX_OUTPUT_BYTES, so
        // copying these bytes into the blocking task is intentionally
        // small and predictable.
        let bytes = bytes.to_vec();

        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut file = AtomicWriteFile::open(&path)?;

            file.write_all(&bytes)?;

            // The destination becomes visible only when commit
            // succeeds.
            file.commit()?;

            Ok(())
        })
        .await
        .map_err(|error| ApiError::Internal(format!("Atomic image write task failed: {error}")))?
        .map_err(|error| ApiError::Io(error.to_string()))?;

        Ok(())
    }

    /// Reads a previously stored WebP image.
    pub async fn read(&self, key: &ImageKey) -> Result<Vec<u8>, ApiError> {
        let path = self.image_path(key);

        match fs::read(path).await {
            Ok(bytes) => Ok(bytes),

            Err(error) if error.kind() == ErrorKind::NotFound => {
                Err(ApiError::NotFound("Image not found".to_string()))
            }

            Err(error) => Err(ApiError::Io(error.to_string())),
        }
    }

    /// Resolves the physical filesystem path corresponding to an image
    /// identifier.
    fn image_path(&self, key: &ImageKey) -> PathBuf {
        physical_image_path(&self.root, key)
    }
}

/// Builds the physical path for an image.
///
/// Keeping this operation centralized prevents handlers and services
/// from making assumptions about the storage layout.
fn physical_image_path(root: &Path, key: &ImageKey) -> PathBuf {
    root.join(&key.namespace)
        .join(&key.resource_id)
        .join(format!("{:02}.webp", key.slot))
}
