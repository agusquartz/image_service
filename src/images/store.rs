use crate::{config::MAX_SLOT_NUMBER, error::ApiError};

use opendal::{EntryMode, ErrorKind, Operator, raw::PresignedRequest};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, SystemTime},
};
use tokio::sync::Mutex;

use super::model::ImageKey;

/// One image slot discovered while listing a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredImage {
    pub slot: u8,
    pub content_length: u64,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredMetadata {
    pub content_length: u64,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
}

/// The actual storage implementation is provided by OpenDAL.
///
/// Currently enabled backends:
///
/// - Local filesystem
/// - S3 and S3-compatible object storage
#[derive(Debug, Clone)]
pub(crate) struct ImageStore {
    operator: Operator,
    /// OpenDAL filesystem conditional writes check existence before rename.
    /// Per-key locks protect that sequence without serializing unrelated slots.
    write_locks: Arc<StdMutex<HashMap<String, Weak<Mutex<()>>>>>,
}

impl ImageStore {
    /// Creates an image store backed by the provided OpenDAL operator.
    pub fn new(operator: Operator) -> Self {
        Self {
            operator,
            write_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    fn write_lock_for(&self, object_key: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .write_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(object_key).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(Mutex::new(()));
        locks.insert(object_key.to_string(), Arc::downgrade(&lock));
        lock
    }

    /// Creates a processed WebP image only if the slot does not exist.
    pub async fn create(
        &self,
        key: &ImageKey,
        bytes: &[u8],
        max_slots: u8,
    ) -> Result<(), ApiError> {
        if key.slot > max_slots {
            return Err(slot_limit_error(key.slot, max_slots));
        }

        let object_key = image_object_key(key);
        let write_lock = self.write_lock_for(&object_key);
        let _write_guard = write_lock.lock().await;

        self.operator
            .write_with(&object_key, bytes.to_vec())
            .if_not_exists(true)
            .await
            .map_err(|error| {
                if error.kind() == ErrorKind::ConditionNotMatch {
                    ApiError::Conflict("An image already exists in this slot".to_string())
                } else {
                    ApiError::Storage(error.to_string())
                }
            })?;

        Ok(())
    }

    /// Stores a processed WebP image, creating or replacing the slot.
    ///
    /// Returns `true` when the slot was created, and `false` when it was replaced.
    pub async fn write(
        &self,
        key: &ImageKey,
        bytes: &[u8],
        max_slots: u8,
    ) -> Result<bool, ApiError> {
        let object_key = image_object_key(key);
        let write_lock = self.write_lock_for(&object_key);
        let _write_guard = write_lock.lock().await;

        if key.slot > max_slots {
            match self.operator.stat(&object_key).await {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    return Err(slot_limit_error(key.slot, max_slots));
                }
                Err(error) => return Err(ApiError::Storage(error.to_string())),
            }
        }

        match self
            .operator
            .write_with(&object_key, bytes.to_vec())
            .if_not_exists(true)
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::ConditionNotMatch => {
                self.operator
                    .write(&object_key, bytes.to_vec())
                    .await
                    .map_err(|error| ApiError::Storage(error.to_string()))?;
                Ok(false)
            }
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
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

    /// Deletes an image. Missing objects are treated as successful deletion.
    pub async fn delete(&self, key: &ImageKey) -> Result<(), ApiError> {
        let object_key = image_object_key(key);
        let write_lock = self.write_lock_for(&object_key);
        let _write_guard = write_lock.lock().await;

        match self.operator.delete(&object_key).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    /// Deletes every object belonging to a resource.
    ///
    /// The trailing slash keeps similarly named resources isolated. OpenDAL
    /// handles the filesystem directory and object-storage prefix semantics
    /// so callers do not need backend-specific behavior.
    pub async fn delete_resource(
        &self,
        namespace: &str,
        resource_id: &str,
    ) -> Result<(), ApiError> {
        let prefix = format!("{namespace}/{resource_id}/");
        self.operator
            .delete_with(&prefix)
            .recursive(true)
            .await
            .map_err(|error| ApiError::Storage(error.to_string()))
    }

    /// Returns available storage metadata, or not-found when the slot is absent.
    pub async fn stat(&self, key: &ImageKey) -> Result<StoredMetadata, ApiError> {
        self.operator
            .stat(&image_object_key(key))
            .await
            .map(|metadata| StoredMetadata {
                content_length: metadata.content_length(),
                etag: metadata.etag().map(str::to_string),
                last_modified: metadata.last_modified().map(SystemTime::from),
            })
            .map_err(|error| {
                if error.kind() == ErrorKind::NotFound {
                    ApiError::NotFound("Image not found".to_string())
                } else {
                    ApiError::Storage(error.to_string())
                }
            })
    }

    /// Creates a presigned GET request when the storage backend supports it.
    pub async fn presign_read(
        &self,
        key: &ImageKey,
        expires_in: Duration,
    ) -> Result<PresignedRequest, ApiError> {
        if !self.operator.info().full_capability().presign_read {
            return Err(ApiError::NotImplemented(
                "Presigned reads are not supported by this storage backend".to_string(),
            ));
        }

        self.operator
            .presign_read(&image_object_key(key), expires_in)
            .await
            .map_err(|error| {
                if error.kind() == ErrorKind::Unsupported {
                    ApiError::NotImplemented(
                        "Presigned reads are not supported by this storage backend".to_string(),
                    )
                } else if error.kind() == ErrorKind::NotFound {
                    ApiError::NotFound("Image not found".to_string())
                } else {
                    ApiError::Storage(error.to_string())
                }
            })
    }

    /// Lists valid image slots stored under a namespace/resource prefix.
    pub async fn list_resource(
        &self,
        namespace: &str,
        resource_id: &str,
    ) -> Result<Vec<StoredImage>, ApiError> {
        let prefix = format!("{namespace}/{resource_id}/");
        let entries = match self.operator.list(&prefix).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ApiError::Storage(error.to_string())),
        };

        let mut images = entries
            .into_iter()
            .filter_map(|entry| {
                if entry.metadata().mode() != EntryMode::FILE {
                    return None;
                }
                let path = entry.path();
                let filename = path.strip_prefix(&prefix)?;
                let slot_text = filename.strip_suffix(".webp")?;
                if slot_text.is_empty() || !slot_text.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                let slot = slot_text.parse::<u8>().ok()?;
                if !(1..=MAX_SLOT_NUMBER).contains(&slot) {
                    return None;
                }
                let metadata = entry.metadata();
                Some(StoredImage {
                    slot,
                    content_length: metadata.content_length(),
                    etag: metadata.etag().map(str::to_string),
                    last_modified: metadata.last_modified().map(SystemTime::from),
                })
            })
            .collect::<Vec<_>>();

        images.sort_by_key(|image| image.slot);
        images.dedup_by_key(|image| image.slot);
        Ok(images)
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

fn slot_limit_error(slot: u8, max_slots: u8) -> ApiError {
    ApiError::BadRequest(format!(
        "cannot create slot {slot}: IMAGE_MAX_SLOTS is {max_slots}"
    ))
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
            .write(&key, expected, 5)
            .await
            .expect("Image write should succeed");

        let actual = store.read(&key).await.expect("Image read should succeed");

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn create_rejects_existing_slot_without_replacing_it() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .create(&key, b"original image", 5)
            .await
            .expect("First create should succeed");

        let error = store
            .create(&key, b"second image", 5)
            .await
            .expect_err("Second create must not overwrite the image");
        assert!(matches!(error, ApiError::Conflict(_)));

        let stored = store.read(&key).await.expect("Original should remain");
        assert_eq!(stored, b"original image");
    }

    #[tokio::test]
    async fn concurrent_creates_allow_only_one_image_to_claim_a_slot() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        let (first, second) = tokio::join!(
            store.create(&key, b"first image", 5),
            store.create(&key, b"second image", 5),
        );

        let results = [first, second];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(ApiError::Conflict(_))))
                .count(),
            1
        );

        let stored = store.read(&key).await.expect("Winner's image remains");
        assert!(stored == b"first image" || stored == b"second image");
    }

    #[tokio::test]
    async fn replaces_existing_image_in_same_slot() {
        let (_temp_dir, store) = test_store();

        let key = test_key(1);

        store
            .write(&key, b"first image", 5)
            .await
            .expect("Initial image write should succeed");

        store
            .write(&key, b"replacement image", 5)
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
            .write(&key, expected, 5)
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

    #[tokio::test]
    async fn stats_image_size_and_last_modified() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .write(&key, b"webp bytes", 5)
            .await
            .expect("Image write should succeed");

        let metadata = store.stat(&key).await.expect("Stat should succeed");

        assert_eq!(metadata.content_length, 10);
        assert!(metadata.last_modified.is_some());
    }

    #[tokio::test]
    async fn lists_slots_in_numeric_order_and_deletes_idempotently() {
        let (_temp_dir, store) = test_store();
        for slot in [5, 2, 1] {
            store
                .write(&test_key(slot), b"webp bytes", 5)
                .await
                .expect("Image write should succeed");
        }

        let images = store
            .list_resource("products", "845")
            .await
            .expect("List should succeed");
        assert_eq!(
            images.iter().map(|image| image.slot).collect::<Vec<_>>(),
            [1, 2, 5]
        );

        store
            .delete(&test_key(2))
            .await
            .expect("Delete should succeed");
        store
            .delete(&test_key(2))
            .await
            .expect("Deleting a missing image should be idempotent");
        let images = store
            .list_resource("products", "845")
            .await
            .expect("List should succeed");
        assert_eq!(
            images.iter().map(|image| image.slot).collect::<Vec<_>>(),
            [1, 5]
        );
    }

    #[tokio::test]
    async fn deletes_all_objects_for_one_resource_without_touching_prefix_siblings() {
        let (_temp_dir, store) = test_store();
        for (resource_id, slot) in [("845", 1), ("845", 2), ("845-extra", 1)] {
            let key = ImageKey::new("products".to_string(), resource_id.to_string(), slot)
                .expect("test image key should be valid");
            store
                .write(&key, b"webp bytes", 5)
                .await
                .expect("Image write should succeed");
        }

        store
            .delete_resource("products", "845")
            .await
            .expect("Resource delete should succeed");
        store
            .delete_resource("products", "845")
            .await
            .expect("Deleting a missing resource should be idempotent");

        assert!(
            store
                .list_resource("products", "845")
                .await
                .expect("List should succeed")
                .is_empty()
        );
        assert_eq!(
            store
                .list_resource("products", "845-extra")
                .await
                .expect("Sibling resource list should succeed")
                .iter()
                .map(|image| image.slot)
                .collect::<Vec<_>>(),
            [1]
        );
    }

    #[tokio::test]
    async fn returns_stable_error_when_filesystem_cannot_presign() {
        let (_temp_dir, store) = test_store();

        let error = store
            .presign_read(&test_key(1), std::time::Duration::from_secs(60))
            .await
            .expect_err("Filesystem backend must not support presigned reads");

        match error {
            ApiError::NotImplemented(message) => assert_eq!(
                message,
                "Presigned reads are not supported by this storage backend"
            ),
            other => panic!("Expected NotImplemented error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn creates_presigned_read_request_for_s3_without_network_access() {
        let operator = Operator::new(
            opendal::services::S3::default()
                .bucket("image-service-test")
                .region("us-east-1")
                .endpoint("http://127.0.0.1:9000")
                .access_key_id("test-access-key")
                .secret_access_key("test-secret-key"),
        )
        .expect("S3 builder should be valid")
        .finish();
        let store = ImageStore::new(operator);

        let request = store
            .presign_read(&test_key(1), std::time::Duration::from_secs(600))
            .await
            .expect("Presigning does not contact the S3 endpoint");

        assert_eq!(request.method(), "GET");
        assert!(
            request
                .uri()
                .to_string()
                .starts_with("http://127.0.0.1:9000/")
        );
        assert!(request.uri().query().is_some());
    }
}
