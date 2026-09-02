use crate::error::ApiError;

use opendal::{ErrorKind, Operator};

use super::model::ImageKey;

/// The actual storage implementation is provided by OpenDAL.
///
/// Currently enabled backends:
///
/// - Local filesystem
/// - S3 and S3-compatible object storage
#[derive(Debug, Clone)]
pub(crate) struct ImageStore {
    operator: Operator,
}

impl ImageStore {
    /// Creates an image store backed by the provided OpenDAL operator.
    pub fn new(operator: Operator) -> Self {
        Self { operator }
    }

    /// Stores a processed WebP image.
    ///
    /// If the requested slot already exists, it is replaced.
    pub async fn write(&self, key: &ImageKey, bytes: &[u8]) -> Result<(), ApiError> {
        let object_key = image_object_key(key);

        self.operator
            .write(&object_key, bytes.to_vec())
            .await
            .map_err(|error| ApiError::Storage(error.to_string()))?;

        Ok(())
    }

    /// Reads a previously stored WebP image.
    pub async fn read(&self, key: &ImageKey) -> Result<Vec<u8>, ApiError> {
        let object_key = image_object_key(key);

        match self.operator.read(&object_key).await {
            Ok(buffer) => Ok(buffer.to_vec()),

            Err(error) if error.kind() == ErrorKind::NotFound => {
                Err(ApiError::NotFound("Image not found".to_string()))
            }

            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }
}

/// Builds the backend-independent storage key for an image.
///
/// Example:
///
/// `products/845/01.webp`
fn image_object_key(key: &ImageKey) -> String {
    format!("{}/{}/{:02}.webp", key.namespace, key.resource_id, key.slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use tempfile::TempDir;

    /// Creates an isolated filesystem-backed OpenDAL image store.
    ///
    /// Each test gets its own temporary directory, so tests do not
    /// modify the user's real image storage.
    fn test_store() -> (TempDir, ImageStore) {
        let temp_dir = tempfile::tempdir().expect("Could not create temporary directory");

        let root = temp_dir.path().join("images");
        let atomic_write_dir = temp_dir.path().join("tmp");

        let options = HashMap::from([
            ("root".to_string(), root.to_string_lossy().into_owned()),
            (
                "atomic_write_dir".to_string(),
                atomic_write_dir.to_string_lossy().into_owned(),
            ),
        ]);

        let operator =
            Operator::via_iter("fs", options).expect("Could not create filesystem operator");

        (temp_dir, ImageStore::new(operator))
    }

    fn test_key(slot: u8) -> ImageKey {
        ImageKey::new("products".to_string(), "845".to_string(), slot)
            .expect("Test image key should be valid")
    }

    #[test]
    fn builds_backend_independent_object_key() {
        let key = test_key(1);

        let object_key = image_object_key(&key);

        assert_eq!(object_key, "products/845/01.webp");
    }

    #[test]
    fn formats_slots_with_two_digits() {
        let key = test_key(5);

        let object_key = image_object_key(&key);

        assert_eq!(object_key, "products/845/05.webp");
    }

    #[tokio::test]
    async fn writes_and_reads_image() {
        let (_temp_dir, store) = test_store();

        let key = test_key(1);

        let expected = b"processed webp bytes";

        store
            .write(&key, expected)
            .await
            .expect("Image write should succeed");

        let actual = store.read(&key).await.expect("Image read should succeed");

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn replaces_existing_image_in_same_slot() {
        let (_temp_dir, store) = test_store();

        let key = test_key(1);

        store
            .write(&key, b"first image")
            .await
            .expect("Initial image write should succeed");

        store
            .write(&key, b"replacement image")
            .await
            .expect("Replacement image write should succeed");

        let actual = store.read(&key).await.expect("Image read should succeed");

        assert_eq!(actual, b"replacement image");
    }

    #[tokio::test]
    async fn returns_not_found_for_missing_image() {
        let (_temp_dir, store) = test_store();

        let key = test_key(1);

        let result = store.read(&key).await;

        match result {
            Err(ApiError::NotFound(message)) => {
                assert_eq!(message, "Image not found");
            }

            Err(other) => {
                panic!("Expected NotFound error, got: {other:?}");
            }

            Ok(_) => {
                panic!("Expected missing image to return NotFound");
            }
        }
    }

    #[tokio::test]
    async fn stores_image_using_expected_filesystem_layout() {
        let (temp_dir, store) = test_store();

        let key = test_key(1);

        let expected = b"processed webp bytes";

        store
            .write(&key, expected)
            .await
            .expect("Image write should succeed");

        let physical_path = temp_dir
            .path()
            .join("images")
            .join("products")
            .join("845")
            .join("01.webp");

        let actual = tokio::fs::read(physical_path)
            .await
            .expect("Stored image should exist on filesystem");

        assert_eq!(actual, expected);
    }
}
