use crate::{config::MAX_SLOT_NUMBER, error::ApiError};

use axum::body::Bytes;
use futures_util::StreamExt;
use opendal::{EntryMode, ErrorKind, Operator, raw::PresignedRequest};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, SystemTime},
};
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::model::ImageKey;

/// One image slot discovered while listing a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredImage {
    pub slot: u8,
    pub content_length: u64,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredMetadata {
    pub content_length: u64,
    pub etag: Option<String>,
    pub last_modified: Option<SystemTime>,
}

#[derive(Debug, Default)]
struct ResourceOperationState {
    generation: u64,
    slot_generations: HashMap<u8, u64>,
}

/// Captures the resource and slot generations when an upload starts reading.
/// Deletes advance a generation, preventing an older in-flight upload from
/// publishing after the delete has completed.
#[derive(Debug, Clone)]
pub(crate) struct ImageOperationToken {
    state: Arc<Mutex<ResourceOperationState>>,
    resource_generation: u64,
    slot_generation: u64,
    slot: u8,
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
    /// Weak references let idle resource locks disappear while uploads hold a
    /// strong reference to the lock from their admission through publication.
    operation_locks: Arc<StdMutex<HashMap<String, Weak<Mutex<ResourceOperationState>>>>>,
}

impl ImageStore {
    /// Creates an image store backed by the provided OpenDAL operator.
    pub fn new(operator: Operator) -> Self {
        Self {
            operator,
            operation_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    fn operation_lock_for_resource(
        &self,
        namespace: &str,
        resource_id: &str,
    ) -> Arc<Mutex<ResourceOperationState>> {
        let resource_key = format!("{namespace}/{resource_id}");
        let mut locks = self
            .operation_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&resource_key).and_then(Weak::upgrade) {
            return lock;
        }

        let lock = Arc::new(Mutex::new(ResourceOperationState::default()));
        locks.insert(resource_key, Arc::downgrade(&lock));
        lock
    }

    /// Captures the operation generation before an upload consumes its body.
    pub async fn begin_operation(&self, key: &ImageKey) -> ImageOperationToken {
        let state = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
        let guard = state.clone().lock_owned().await;
        ImageOperationToken {
            state,
            resource_generation: guard.generation,
            slot_generation: *guard.slot_generations.get(&key.slot).unwrap_or(&0),
            slot: key.slot,
        }
    }

    async fn lock_current_operation(
        &self,
        key: &ImageKey,
        token: &ImageOperationToken,
    ) -> Result<OwnedMutexGuard<ResourceOperationState>, ApiError> {
        if token.slot != key.slot
            || !Arc::ptr_eq(
                &token.state,
                &self.operation_lock_for_resource(&key.namespace, &key.resource_id),
            )
        {
            return Err(ApiError::Internal(
                "Image operation token does not match the requested slot".to_string(),
            ));
        }

        let guard = token.state.clone().lock_owned().await;
        if guard.generation != token.resource_generation
            || *guard.slot_generations.get(&key.slot).unwrap_or(&0) != token.slot_generation
        {
            return Err(ApiError::Conflict(
                "The image operation was superseded by a delete".to_string(),
            ));
        }
        Ok(guard)
    }

    /// Fast create-only preflight. The final conditional write remains the
    /// authority because another request can create the slot after this check.
    pub async fn ensure_absent(&self, key: &ImageKey) -> Result<(), ApiError> {
        match self.operator.stat(&image_object_key(key)).await {
            Ok(_) => Err(ApiError::Conflict(
                "An image already exists in this slot".to_string(),
            )),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    /// Creates a processed WebP image only if the slot does not exist.
    #[cfg(test)]
    pub async fn create(
        &self,
        key: &ImageKey,
        bytes: Bytes,
        max_slots: u8,
    ) -> Result<(), ApiError> {
        let token = self.begin_operation(key).await;
        self.create_with_token(key, bytes, max_slots, &token)
            .await
            .map(|_| ())
    }

    pub async fn create_with_token(
        &self,
        key: &ImageKey,
        bytes: Bytes,
        max_slots: u8,
        token: &ImageOperationToken,
    ) -> Result<StoredMetadata, ApiError> {
        if key.slot > max_slots {
            return Err(slot_limit_error(key.slot, max_slots));
        }

        let _operation_guard = self.lock_current_operation(key, token).await?;
        let object_key = image_object_key(key);

        let metadata = self
            .operator
            .write_with(&object_key, bytes)
            .if_not_exists(true)
            .await
            .map_err(|error| {
                if error.kind() == ErrorKind::ConditionNotMatch {
                    ApiError::Conflict("An image already exists in this slot".to_string())
                } else {
                    ApiError::Storage(error.to_string())
                }
            })?;

        Ok(stored_metadata_from(metadata))
    }

    /// Stores a processed WebP image, creating or replacing the slot.
    ///
    /// Returns `true` when the slot was created, and `false` when it was replaced.
    #[cfg(test)]
    pub async fn write(
        &self,
        key: &ImageKey,
        bytes: Bytes,
        max_slots: u8,
    ) -> Result<bool, ApiError> {
        let token = self.begin_operation(key).await;
        self.write_with_token(key, bytes, max_slots, &token)
            .await
            .map(|(created, _)| created)
    }

    pub async fn write_with_token(
        &self,
        key: &ImageKey,
        bytes: Bytes,
        max_slots: u8,
        token: &ImageOperationToken,
    ) -> Result<(bool, StoredMetadata), ApiError> {
        let _operation_guard = self.lock_current_operation(key, token).await?;
        let object_key = image_object_key(key);

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
            .write_with(&object_key, bytes.clone())
            .if_not_exists(true)
            .await
        {
            Ok(metadata) => Ok((true, stored_metadata_from(metadata))),
            Err(error) if error.kind() == ErrorKind::ConditionNotMatch => {
                let metadata = self
                    .operator
                    .write(&object_key, bytes)
                    .await
                    .map_err(|error| ApiError::Storage(error.to_string()))?;
                Ok((false, stored_metadata_from(metadata)))
            }
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    /// Reads a consistent metadata/body snapshot for one image.
    ///
    /// The resource lock excludes writes and deletes made by this process.
    /// Backends that support conditional reads also verify the ETag so writes
    /// from another process cannot pair stale metadata with newer bytes.
    pub async fn read_snapshot(
        &self,
        key: &ImageKey,
    ) -> Result<(StoredMetadata, Vec<u8>), ApiError> {
        let object_key = image_object_key(key);
        let supports_if_match = self.operator.info().full_capability().read_with_if_match;

        if !supports_if_match {
            // LocalFS has no conditional-read primitive, so serialize the
            // stat/read pair against mutations made by this process.
            let state = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
            let _operation_guard = state.lock_owned().await;
            let metadata = self.stat(key).await?;
            let bytes = self.read_object(&object_key).await?;
            return Ok((metadata, bytes));
        }

        // S3-compatible backends can validate the ETag atomically with GET.
        // That keeps same-resource readers concurrent while still preventing
        // a write from another process from mixing response metadata and bytes.
        for _ in 0..3 {
            let metadata = self.stat(key).await?;
            if let Some(etag) = metadata.etag.as_deref() {
                match self.operator.read_with(&object_key).if_match(etag).await {
                    Ok(buffer) => return Ok((metadata, buffer.to_vec())),
                    Err(error) if error.kind() == ErrorKind::ConditionNotMatch => continue,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        return Err(ApiError::NotFound("Image not found".to_string()));
                    }
                    Err(error) => return Err(ApiError::Storage(error.to_string())),
                }
            }

            // A backend may advertise If-Match but omit ETags for a particular
            // object. Use the process-local lock for that fallback case.
            let state = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
            let _operation_guard = state.lock_owned().await;
            let current_metadata = self.stat(key).await?;
            if current_metadata.etag.is_some() {
                continue;
            }
            let bytes = self.read_object(&object_key).await?;
            return Ok((current_metadata, bytes));
        }

        Err(ApiError::ServiceUnavailable(
            "Image changed repeatedly while being read; retry the request".to_string(),
        ))
    }

    /// Opens a reader pinned to the same object version represented by metadata.
    ///
    /// Object stores use If-Match when available. LocalFS opens the file while
    /// holding the in-process resource lock; its atomic replacement keeps that
    /// open handle on the previous inode after the lock is released.
    pub async fn open_image_reader(
        &self,
        key: &ImageKey,
        chunk_bytes: usize,
    ) -> Result<(StoredMetadata, opendal::Reader), ApiError> {
        let object_key = image_object_key(key);
        let supports_if_match = self.operator.info().full_capability().read_with_if_match;

        for _ in 0..3 {
            let metadata = self.stat(key).await?;
            if supports_if_match && let Some(etag) = metadata.etag.as_deref() {
                match self
                    .operator
                    .reader_with(&object_key)
                    .if_match(etag)
                    .chunk(chunk_bytes)
                    .await
                {
                    Ok(reader) => return Ok((metadata, reader)),
                    Err(error) if error.kind() == ErrorKind::ConditionNotMatch => continue,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        return Err(ApiError::NotFound("Image not found".to_string()));
                    }
                    Err(error) => return Err(ApiError::Storage(error.to_string())),
                }
            }

            let resource_lock = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
            let _guard = resource_lock.lock_owned().await;
            let current = self.stat(key).await?;
            if current.etag != metadata.etag
                || current.content_length != metadata.content_length
                || current.last_modified != metadata.last_modified
            {
                continue;
            }
            let reader = self
                .operator
                .reader_with(&object_key)
                .chunk(chunk_bytes)
                .await
                .map_err(|error| {
                    if error.kind() == ErrorKind::NotFound {
                        ApiError::NotFound("Image not found".to_string())
                    } else {
                        ApiError::Storage(error.to_string())
                    }
                })?;
            return Ok((current, reader));
        }

        Err(ApiError::ServiceUnavailable(
            "Image changed repeatedly while opening a download; retry the request".to_string(),
        ))
    }

    /// Opens durable staging for one immutable Tus PATCH chunk.
    pub async fn open_resumable_chunk_writer(
        &self,
        session_id: &str,
        chunk_id: &str,
    ) -> Result<opendal::Writer, ApiError> {
        self.operator
            .writer(&resumable_chunk_key(session_id, chunk_id))
            .await
            .map_err(|error| ApiError::Storage(error.to_string()))
    }

    pub async fn read_resumable_chunk(
        &self,
        session_id: &str,
        chunk_id: &str,
    ) -> Result<Bytes, ApiError> {
        self.operator
            .read(&resumable_chunk_key(session_id, chunk_id))
            .await
            .map(|buffer| buffer.to_bytes())
            .map_err(|error| {
                if error.kind() == ErrorKind::NotFound {
                    ApiError::ServiceUnavailable(
                        "A confirmed upload chunk is missing from durable staging".to_string(),
                    )
                } else {
                    ApiError::Storage(error.to_string())
                }
            })
    }

    pub async fn delete_resumable_chunk(
        &self,
        session_id: &str,
        chunk_id: &str,
    ) -> Result<(), ApiError> {
        match self
            .operator
            .delete(&resumable_chunk_key(session_id, chunk_id))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    pub async fn delete_resumable_upload_staging(&self, session_id: &str) -> Result<(), ApiError> {
        let prefix = resumable_session_prefix(session_id);
        let mut lister = match self.operator.lister_with(&prefix).recursive(true).await {
            Ok(lister) => lister,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(ApiError::Storage(error.to_string())),
        };
        while let Some(entry) = lister.next().await {
            let entry = entry.map_err(|error| ApiError::Storage(error.to_string()))?;
            if entry.metadata().mode() != EntryMode::FILE {
                continue;
            }
            self.operator
                .delete(entry.path())
                .await
                .map_err(|error| ApiError::Storage(error.to_string()))?;
        }
        Ok(())
    }

    pub async fn list_resumable_staging_keys(&self) -> Result<Vec<String>, ApiError> {
        let mut lister = match self
            .operator
            .lister_with("__image_transfer/resumable/")
            .recursive(true)
            .await
        {
            Ok(lister) => lister,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ApiError::Storage(error.to_string())),
        };
        let mut keys = Vec::new();
        while let Some(entry) = lister.next().await {
            let entry = entry.map_err(|error| ApiError::Storage(error.to_string()))?;
            if entry.metadata().mode() == EntryMode::FILE {
                keys.push(entry.path().to_string());
            }
        }
        Ok(keys)
    }

    pub async fn delete_resumable_staging_key(&self, key: &str) -> Result<(), ApiError> {
        if !key.starts_with("__image_transfer/resumable/") {
            return Err(ApiError::Internal(
                "Refusing to delete a key outside resumable staging".into(),
            ));
        }
        self.operator.delete(key).await.map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                ApiError::NotFound("Staged chunk not found".into())
            } else {
                ApiError::Storage(error.to_string())
            }
        })
    }

    /// Confirms that metadata used for a conditional response still identifies
    /// the current object. S3-compatible stores verify the ETag with a one-byte
    /// conditional range read; LocalFS checks under the resource operation lock.
    pub async fn is_current_snapshot(
        &self,
        key: &ImageKey,
        metadata: &StoredMetadata,
    ) -> Result<bool, ApiError> {
        let object_key = image_object_key(key);
        let supports_if_match = self.operator.info().full_capability().read_with_if_match;

        if supports_if_match {
            let Some(etag) = metadata.etag.as_deref() else {
                // Without a validator, force the caller to fetch the full
                // snapshot and evaluate its content-derived ETag instead.
                return Ok(false);
            };
            return match self
                .operator
                .read_with(&object_key)
                .if_match(etag)
                .range(0..1)
                .await
            {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == ErrorKind::ConditionNotMatch => Ok(false),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    Err(ApiError::NotFound("Image not found".to_string()))
                }
                Err(error) => Err(ApiError::Storage(error.to_string())),
            };
        }

        let state = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
        let _operation_guard = state.lock_owned().await;
        let current = self.stat(key).await?;
        Ok(current.etag == metadata.etag
            && current.content_length == metadata.content_length
            && current.last_modified == metadata.last_modified)
    }

    /// Replaces an object only while it still matches the version that was read.
    /// Object stores use conditional writes; LocalFS serializes the comparison
    /// and write against mutations made by this process.
    pub async fn replace_if_unchanged(
        &self,
        key: &ImageKey,
        expected: &StoredMetadata,
        bytes: Bytes,
    ) -> Result<bool, ApiError> {
        let object_key = image_object_key(key);
        if self.operator.info().full_capability().write_with_if_match {
            let Some(etag) = expected.etag.as_deref() else {
                return Ok(false);
            };
            return match self
                .operator
                .write_with(&object_key, bytes)
                .if_match(etag)
                .await
            {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == ErrorKind::ConditionNotMatch => Ok(false),
                Err(error) => Err(ApiError::Storage(error.to_string())),
            };
        }

        let state = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
        let _operation_guard = state.lock_owned().await;
        let current = match self.stat(key).await {
            Ok(metadata) => metadata,
            Err(ApiError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        if current.content_length != expected.content_length
            || current.etag != expected.etag
            || current.last_modified != expected.last_modified
        {
            return Ok(false);
        }
        self.operator
            .write(&object_key, bytes)
            .await
            .map_err(|error| ApiError::Storage(error.to_string()))?;
        Ok(true)
    }

    async fn read_object(&self, object_key: &str) -> Result<Vec<u8>, ApiError> {
        match self.operator.read(object_key).await {
            Ok(buffer) => Ok(buffer.to_vec()),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Err(ApiError::NotFound("Image not found".to_string()))
            }
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    /// Reads a previously stored WebP image.
    #[cfg(test)]
    pub async fn read(&self, key: &ImageKey) -> Result<Vec<u8>, ApiError> {
        self.read_object(&image_object_key(key)).await
    }

    pub async fn open_staged_writer(&self, staging_id: &str) -> Result<opendal::Writer, ApiError> {
        self.operator
            .writer(&staging_object_key(staging_id))
            .await
            .map_err(|error| ApiError::Storage(error.to_string()))
    }

    pub async fn open_transfer_writer(
        &self,
        staging_id: &str,
    ) -> Result<opendal::Writer, ApiError> {
        self.operator
            .writer(&transfer_staging_object_key(staging_id))
            .await
            .map_err(|error| ApiError::Storage(error.to_string()))
    }

    pub async fn read_transfer_staged(&self, staging_id: &str) -> Result<Bytes, ApiError> {
        match self
            .operator
            .read(&transfer_staging_object_key(staging_id))
            .await
        {
            Ok(buffer) => Ok(buffer.to_bytes()),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Err(ApiError::NotFound("Staged upload not found".to_string()))
            }
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    pub async fn delete_transfer_staged(&self, staging_id: &str) -> Result<(), ApiError> {
        let key = transfer_staging_object_key(staging_id);
        match self.operator.delete(&key).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    pub async fn read_staged(&self, staging_id: &str) -> Result<Vec<u8>, ApiError> {
        self.read_object(&staging_object_key(staging_id)).await
    }

    pub async fn delete_staged(&self, staging_id: &str) -> Result<(), ApiError> {
        let key = staging_object_key(staging_id);
        match self.operator.delete(&key).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApiError::Storage(error.to_string())),
        }
    }

    pub async fn list_staged_ids(&self) -> Result<Vec<String>, ApiError> {
        let mut lister = match self
            .operator
            .lister_with("__image_jobs/")
            .recursive(true)
            .await
        {
            Ok(lister) => lister,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ApiError::Storage(error.to_string())),
        };
        let mut ids = Vec::new();
        while let Some(entry) = lister.next().await {
            let entry = entry.map_err(|error| ApiError::Storage(error.to_string()))?;
            let Some(id) = entry
                .path()
                .strip_prefix("__image_jobs/")
                .and_then(|path| path.split('/').next())
            else {
                continue;
            };
            if !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Deletes an image. Missing objects are treated as successful deletion.
    pub async fn delete(&self, key: &ImageKey) -> Result<(), ApiError> {
        let state = self.operation_lock_for_resource(&key.namespace, &key.resource_id);
        let mut operation_guard = state.lock_owned().await;
        let generation = operation_guard
            .slot_generations
            .entry(key.slot)
            .or_default();
        *generation = generation.wrapping_add(1);

        let object_key = image_object_key(key);

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
        let state = self.operation_lock_for_resource(namespace, resource_id);
        let mut operation_guard = state.lock_owned().await;
        operation_guard.generation = operation_guard.generation.wrapping_add(1);

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

fn staging_object_key(staging_id: &str) -> String {
    format!("__image_jobs/{staging_id}/original")
}

fn transfer_staging_object_key(staging_id: &str) -> String {
    format!("__image_transfer/{staging_id}/source")
}

fn resumable_session_prefix(session_id: &str) -> String {
    format!("__image_transfer/resumable/{session_id}/")
}

fn resumable_chunk_key(session_id: &str, chunk_id: &str) -> String {
    format!("{}chunks/{chunk_id}", resumable_session_prefix(session_id))
}

fn stored_metadata_from(metadata: opendal::Metadata) -> StoredMetadata {
    StoredMetadata {
        content_length: metadata.content_length(),
        etag: metadata.etag().map(str::to_owned),
        last_modified: metadata.last_modified().map(SystemTime::from),
    }
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
            .write(&key, Bytes::from_static(expected), 5)
            .await
            .expect("Image write should succeed");

        let actual = store.read(&key).await.expect("Image read should succeed");

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn transfer_staging_round_trips_and_deletes_independently_of_async_job_staging() {
        let (_temp_dir, store) = test_store();
        let id = uuid::Uuid::new_v4().to_string();
        let data = Bytes::from_static(b"complete staged source");
        let mut writer = store.open_transfer_writer(&id).await.unwrap();
        writer.write(data.clone()).await.unwrap();
        writer.close().await.unwrap();

        assert_eq!(store.read_transfer_staged(&id).await.unwrap(), data);
        assert!(store.list_staged_ids().await.unwrap().is_empty());
        store.delete_transfer_staged(&id).await.unwrap();
        assert!(store.read_transfer_staged(&id).await.is_err());
    }

    #[tokio::test]
    async fn conditional_maintenance_write_never_replaces_a_newer_version() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .write(&key, Bytes::from_static(b"old"), 5)
            .await
            .unwrap();
        let (captured, _) = store.read_snapshot(&key).await.unwrap();

        store
            .write(&key, Bytes::from_static(b"newer-version"), 5)
            .await
            .unwrap();
        let published = store
            .replace_if_unchanged(&key, &captured, Bytes::from_static(b"reprocessed"))
            .await
            .unwrap();

        assert!(!published);
        assert_eq!(store.read(&key).await.unwrap(), b"newer-version");
    }

    #[tokio::test]
    async fn read_snapshot_returns_metadata_for_the_same_body() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        let expected = Bytes::from_static(b"a stored image version");
        store
            .write(&key, expected.clone(), 5)
            .await
            .expect("Image write should succeed");

        let (metadata, actual) = store
            .read_snapshot(&key)
            .await
            .expect("Snapshot read should succeed");

        assert_eq!(actual, expected);
        assert_eq!(metadata.content_length, actual.len() as u64);
        assert!(metadata.last_modified.is_some());
    }

    #[tokio::test]
    async fn stale_metadata_cannot_authorize_a_not_modified_response() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .write(&key, Bytes::from_static(b"old version"), 5)
            .await
            .expect("Initial image write should succeed");
        let stale_metadata = store.stat(&key).await.expect("Image should exist");

        store
            .write(&key, Bytes::from_static(b"new version is longer"), 5)
            .await
            .expect("Image replacement should succeed");

        assert!(
            !store
                .is_current_snapshot(&key, &stale_metadata)
                .await
                .expect("Snapshot validation should succeed")
        );
    }

    #[tokio::test]
    async fn concurrent_replacement_never_mixes_snapshot_metadata_and_body() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .write(&key, Bytes::from(vec![0; 8]), 5)
            .await
            .expect("Initial image write should succeed");

        for version in 1..=32 {
            let replacement = Bytes::from(vec![version; version as usize + 8]);
            let (write_result, snapshot) =
                tokio::join!(store.write(&key, replacement, 5), store.read_snapshot(&key),);
            write_result.expect("Concurrent replacement should succeed");
            let (metadata, body) = snapshot.expect("Concurrent snapshot read should succeed");
            assert_eq!(metadata.content_length, body.len() as u64);
        }
    }

    #[tokio::test]
    async fn create_rejects_existing_slot_without_replacing_it() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .create(&key, Bytes::from_static(b"original image"), 5)
            .await
            .expect("First create should succeed");

        let error = store
            .create(&key, Bytes::from_static(b"second image"), 5)
            .await
            .expect_err("Second create must not overwrite the image");
        assert!(matches!(error, ApiError::Conflict(_)));

        let stored = store.read(&key).await.expect("Original should remain");
        assert_eq!(stored, b"original image");
    }

    #[tokio::test]
    async fn create_preflight_rejects_existing_slot_without_changing_it() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .create(&key, Bytes::from_static(b"original image"), 5)
            .await
            .expect("Initial create should succeed");

        let error = store
            .ensure_absent(&key)
            .await
            .expect_err("Preflight should reject an occupied slot");
        assert!(matches!(error, ApiError::Conflict(_)));
        assert_eq!(
            store.read(&key).await.expect("Original should remain"),
            b"original image"
        );
    }

    #[tokio::test]
    async fn concurrent_creates_allow_only_one_image_to_claim_a_slot() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        let (first, second) = tokio::join!(
            store.create(&key, Bytes::from_static(b"first image"), 5),
            store.create(&key, Bytes::from_static(b"second image"), 5),
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
            .write(&key, Bytes::from_static(b"first image"), 5)
            .await
            .expect("Initial image write should succeed");

        store
            .write(&key, Bytes::from_static(b"replacement image"), 5)
            .await
            .expect("Replacement image write should succeed");

        let actual = store.read(&key).await.expect("Image read should succeed");

        assert_eq!(actual, b"replacement image");
    }

    #[tokio::test]
    async fn slot_delete_invalidates_upload_started_before_the_delete() {
        let (_temp_dir, store) = test_store();
        let key = test_key(1);
        store
            .write(&key, Bytes::from_static(b"old image"), 5)
            .await
            .expect("Initial image should exist");
        let operation = store.begin_operation(&key).await;

        store.delete(&key).await.expect("Delete should succeed");
        let error = store
            .write_with_token(
                &key,
                Bytes::from_static(b"stale replacement"),
                5,
                &operation,
            )
            .await
            .expect_err("Upload begun before delete must not resurrect the slot");

        assert!(matches!(error, ApiError::Conflict(_)));
        assert!(matches!(store.read(&key).await, Err(ApiError::NotFound(_))));
    }

    #[tokio::test]
    async fn resource_delete_invalidates_uploads_started_for_any_slot() {
        let (_temp_dir, store) = test_store();
        let key = test_key(2);
        let operation = store.begin_operation(&key).await;

        store
            .delete_resource("products", "845")
            .await
            .expect("Resource delete should succeed");
        let error = store
            .write_with_token(&key, Bytes::from_static(b"stale image"), 5, &operation)
            .await
            .expect_err("Upload begun before resource delete must be invalidated");

        assert!(matches!(error, ApiError::Conflict(_)));
        assert!(
            store
                .list_resource("products", "845")
                .await
                .expect("List should succeed")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn deleting_one_slot_does_not_invalidate_sibling_slot_upload() {
        let (_temp_dir, store) = test_store();
        let sibling_key = test_key(2);
        let operation = store.begin_operation(&sibling_key).await;

        store
            .delete(&test_key(1))
            .await
            .expect("Deleting a sibling slot should succeed");
        store
            .write_with_token(
                &sibling_key,
                Bytes::from_static(b"sibling image"),
                5,
                &operation,
            )
            .await
            .expect("An unrelated slot operation should remain valid");

        assert_eq!(
            store
                .read(&sibling_key)
                .await
                .expect("Sibling should exist"),
            b"sibling image"
        );
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
            .write(&key, Bytes::from_static(expected), 5)
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
            .write(&key, Bytes::from_static(b"webp bytes"), 5)
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
                .write(&test_key(slot), Bytes::from_static(b"webp bytes"), 5)
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
                .write(&key, Bytes::from_static(b"webp bytes"), 5)
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
