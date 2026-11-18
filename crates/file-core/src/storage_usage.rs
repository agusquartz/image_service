//! Persistent, eventually consistent storage usage catalog.

use crate::{
    auth::require_auth,
    error::ApiError,
    images::validation::{validate_namespace, validate_resource_id},
    state::AppState,
};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use job_scheduler::{
    ActivityTracker, IntervalSchedule, JobContext, JobFuture, JobHandler, JobOutcome, JobScheduler,
    NewJob,
};
use opendal::{EntryMode, Operator};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const INVENTORY_JOB_KIND: &str = "storage.inventory.reconcile";
const INVENTORY_JOB_QUEUE: &str = "storage.inventory";
const INVENTORY_BATCH_SIZE: usize = 250;

#[derive(Clone)]
pub(crate) struct StorageCatalog {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
struct InventoryObject {
    key: String,
    namespace: String,
    resource_id: String,
    slot: u8,
    size_bytes: u64,
    etag: Option<String>,
    modified_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UsageFilter {
    pub namespace: Option<String>,
    pub resource_id: Option<String>,
    pub slot: Option<u8>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UsageResponse {
    scope: UsageScope,
    object_count: u64,
    total_bytes: u64,
    last_scan_started_at_ms: Option<i64>,
    last_scan_completed_at_ms: Option<i64>,
    scan_status: String,
    scan_age_seconds: Option<u64>,
    stale: bool,
}

#[derive(Debug, Serialize)]
struct UsageScope {
    namespace: Option<String>,
    resource_id: Option<String>,
    slot: Option<u8>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReconcileResponse {
    job_id: String,
    status: &'static str,
}

impl StorageCatalog {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(|error| error.to_string())?;
        }
        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| error.to_string())?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|error| error.to_string())?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS storage_objects (
                    scan_id TEXT NOT NULL,
                    object_key TEXT NOT NULL,
                    namespace TEXT NOT NULL,
                    resource_id TEXT NOT NULL,
                    slot INTEGER NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    etag TEXT,
                    modified_at TEXT,
                    PRIMARY KEY(scan_id, object_key)
                );
                CREATE INDEX IF NOT EXISTS storage_objects_scope
                    ON storage_objects(scan_id, namespace, resource_id, slot);
                CREATE TABLE IF NOT EXISTS storage_catalog_state (
                    id INTEGER PRIMARY KEY CHECK(id = 1),
                    scan_status TEXT NOT NULL,
                    last_scan_id TEXT,
                    last_completed_scan_id TEXT,
                    last_scan_started_ms INTEGER,
                    last_scan_completed_ms INTEGER,
                    last_error TEXT
                );
                INSERT OR IGNORE INTO storage_catalog_state(id, scan_status)
                    VALUES (1, 'never');",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    async fn begin_scan(&self, scan_id: &str) -> Result<(), String> {
        let connection = self.connection.clone();
        let scan_id = scan_id.to_string();
        run_db(connection, move |connection| {
            let active_scan: Option<String> = connection.query_row(
                "SELECT last_scan_id FROM storage_catalog_state WHERE id=1 AND scan_status='running'",
                [],
                |row| row.get(0),
            ).optional()?;
            if active_scan.as_deref().is_some_and(|active| active != scan_id) {
                return Err(rusqlite::Error::ExecuteReturnedResults);
            }
            connection.execute(
                "UPDATE storage_catalog_state SET scan_status='running', last_scan_id=?1, last_scan_started_ms=?2, last_error=NULL WHERE id=1",
                params![scan_id, now_ms()],
            )?;
            Ok(())
        }).await
    }

    async fn upsert_batch(&self, scan_id: &str, batch: Vec<InventoryObject>) -> Result<(), String> {
        if batch.is_empty() {
            return Ok(());
        }
        let connection = self.connection.clone();
        let scan_id = scan_id.to_string();
        run_db(connection, move |connection| {
            let transaction = connection.unchecked_transaction()?;
            for object in batch {
                transaction.execute(
                    "INSERT INTO storage_objects(scan_id, object_key, namespace, resource_id, slot, size_bytes, etag, modified_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(scan_id, object_key) DO UPDATE SET namespace=excluded.namespace, resource_id=excluded.resource_id, slot=excluded.slot, size_bytes=excluded.size_bytes, etag=excluded.etag, modified_at=excluded.modified_at",
                    params![scan_id, object.key, object.namespace, object.resource_id, object.slot, object.size_bytes, object.etag, object.modified_at],
                )?;
            }
            transaction.commit()?;
            Ok(())
        }).await
    }

    async fn finish_scan(&self, scan_id: &str) -> Result<(), String> {
        let connection = self.connection.clone();
        let scan_id = scan_id.to_string();
        run_db(connection, move |connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute("DELETE FROM storage_objects WHERE scan_id <> ?1", [&scan_id])?;
            transaction.execute(
                "UPDATE storage_catalog_state SET scan_status='complete', last_completed_scan_id=?2, last_scan_completed_ms=?1, last_error=NULL WHERE id=1 AND last_scan_id=?2",
                params![now_ms(), scan_id],
            )?;
            transaction.commit()?;
            Ok(())
        }).await
    }

    async fn fail_scan(&self, scan_id: &str, error: &str) {
        let connection = self.connection.clone();
        let scan_id = scan_id.to_string();
        let error = error.chars().take(512).collect::<String>();
        let _ = run_db(connection, move |connection| {
            connection.execute(
                "UPDATE storage_catalog_state SET scan_status='failed', last_error=?1 WHERE id=1 AND last_scan_id=?2",
                params![error, scan_id],
            )?;
            Ok(())
        }).await;
    }

    async fn usage(
        &self,
        filter: UsageFilter,
        interval_seconds: u64,
    ) -> Result<UsageResponse, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
        let (object_count, total_bytes): (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM storage_objects WHERE scan_id=(SELECT last_completed_scan_id FROM storage_catalog_state WHERE id=1) AND (?1 IS NULL OR namespace=?1) AND (?2 IS NULL OR resource_id=?2) AND (?3 IS NULL OR slot=?3)",
                params![filter.namespace, filter.resource_id, filter.slot],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            ?;
        let (scan_status, started_ms, completed_ms): (String, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT scan_status, last_scan_started_ms, last_scan_completed_ms FROM storage_catalog_state WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            ?
            .unwrap_or_else(|| ("never".to_string(), None, None));
        let age_seconds = completed_ms.map(|completed| {
            now_ms().saturating_sub(completed).max(0) as u64 / 1000
        });
        let stale = age_seconds.is_none_or(|age| age > interval_seconds.saturating_mul(2));
        Ok(UsageResponse {
            scope: UsageScope {
                namespace: filter.namespace,
                resource_id: filter.resource_id,
                slot: filter.slot,
            },
            object_count: object_count.max(0) as u64,
            total_bytes: total_bytes.max(0) as u64,
            last_scan_started_at_ms: started_ms,
            last_scan_completed_at_ms: completed_ms,
            scan_status,
            scan_age_seconds: age_seconds,
            stale,
        })
        }).await
    }
}

pub(crate) async fn get_usage(
    State(state): State<AppState>,
    Query(filter): Query<UsageFilter>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_usage_auth(&headers, &state)?;
    if let Some(namespace) = &filter.namespace {
        validate_namespace(namespace)?;
    }
    if let Some(resource_id) = &filter.resource_id {
        if filter.namespace.is_none() {
            return Err(ApiError::BadRequest(
                "namespace is required when resource_id is provided".to_string(),
            ));
        }
        validate_resource_id(resource_id)?;
    }
    if filter.slot.is_some() && filter.resource_id.is_none() {
        return Err(ApiError::BadRequest(
            "resource_id is required when slot is provided".to_string(),
        ));
    }
    if let Some(slot) = filter.slot {
        crate::images::validation::validate_slot(slot)?;
    }
    let catalog = state
        .storage_catalog
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("Storage usage reporting is disabled".to_string()))?;
    let usage = catalog
        .usage(filter, state.storage_inventory_interval_seconds)
        .await
        .map_err(ApiError::Internal)?;
    let mut response = Json(usage).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn request_reconciliation(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_usage_auth(&headers, &state)?;
    let store = state.job_scheduler.store();
    let mut job = NewJob::system(INVENTORY_JOB_QUEUE, INVENTORY_JOB_KIND, Value::Null);
    job.dedupe_key = Some("storage-inventory:manual".to_string());
    job.priority = 20;
    let job_id = store
        .enqueue(job)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let mut response = (
        StatusCode::ACCEPTED,
        Json(ReconcileResponse {
            job_id,
            status: "queued",
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

fn require_usage_auth(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    require_auth(
        headers,
        crate::config::AuthRequirement::Required,
        state.api_key.as_deref(),
    )
}

pub(crate) async fn register_jobs(
    scheduler: &JobScheduler,
    catalog: StorageCatalog,
    operator: Operator,
    interval_seconds: u64,
    activity: Arc<ActivityTracker>,
    idle_seconds: u64,
) -> Result<(), String> {
    scheduler
        .register_handler(
            INVENTORY_JOB_KIND,
            Arc::new(InventoryHandler {
                catalog,
                operator,
                activity,
                idle_seconds,
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    scheduler
        .store()
        .register_interval_schedule(IntervalSchedule {
            id: "storage-inventory-periodic".to_string(),
            queue: INVENTORY_JOB_QUEUE.to_string(),
            kind: INVENTORY_JOB_KIND.to_string(),
            payload: Value::Null,
            policy_snapshot: json!({ "scope": "image-webp-slots" }),
            priority: -10,
            max_attempts: 5,
            interval: Duration::from_secs(interval_seconds),
            run_immediately: true,
        })
        .await
        .map_err(|error| error.to_string())
}

struct InventoryHandler {
    catalog: StorageCatalog,
    operator: Operator,
    activity: Arc<ActivityTracker>,
    idle_seconds: u64,
}

impl JobHandler for InventoryHandler {
    fn execute<'a>(&'a self, context: JobContext) -> JobFuture<'a> {
        Box::pin(async move {
            match self.reconcile(&context).await {
                Ok(count) => JobOutcome::Succeeded(json!({ "objects_seen": count })),
                Err(error) => {
                    self.catalog.fail_scan(&context.job.id, &error).await;
                    JobOutcome::Failed {
                        message: error,
                        retryable: true,
                    }
                }
            }
        })
    }
}

impl InventoryHandler {
    async fn reconcile(&self, context: &JobContext) -> Result<u64, String> {
        let scan_id = context.job.id.clone();
        self.catalog.begin_scan(&scan_id).await?;
        let mut lister = self
            .operator
            .lister_with("")
            .recursive(true)
            .await
            .map_err(|error| error.to_string())?;
        let mut batch = Vec::with_capacity(INVENTORY_BATCH_SIZE);
        let mut count = 0_u64;
        while let Some(entry) = lister.next().await {
            if context.cancellation_requested() {
                return Err("Inventory job was cancelled".to_string());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.metadata().mode() != EntryMode::FILE {
                continue;
            }
            let Some(object) = parse_inventory_object(&entry) else {
                continue;
            };
            batch.push(object);
            count += 1;
            if batch.len() >= INVENTORY_BATCH_SIZE {
                self.catalog
                    .upsert_batch(&scan_id, std::mem::take(&mut batch))
                    .await?;
                context
                    .checkpoint(json!({ "objects_seen": count }))
                    .await
                    .map_err(|error| error.to_string())?;
                self.wait_until_idle(context).await?;
                batch = Vec::with_capacity(INVENTORY_BATCH_SIZE);
            }
        }
        self.catalog.upsert_batch(&scan_id, batch).await?;
        self.catalog.finish_scan(&scan_id).await?;
        context
            .checkpoint(json!({ "objects_seen": count, "complete": true }))
            .await
            .map_err(|error| error.to_string())?;
        Ok(count)
    }

    async fn wait_until_idle(&self, context: &JobContext) -> Result<(), String> {
        let quiet_period = Duration::from_secs(self.idle_seconds);
        while !self.activity.is_idle_for(quiet_period) {
            if context.cancellation_requested() {
                return Err("Inventory job was cancelled".to_string());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }
}

fn parse_inventory_object(entry: &opendal::Entry) -> Option<InventoryObject> {
    let key = entry.path().trim_end_matches('/');
    let mut parts = key.split('/');
    let namespace = parts.next()?;
    let resource_id = parts.next()?;
    let filename = parts.next()?;
    if parts.next().is_some() || !filename.ends_with(".webp") {
        return None;
    }
    validate_namespace(namespace).ok()?;
    validate_resource_id(resource_id).ok()?;
    let slot = filename.strip_suffix(".webp")?.parse::<u8>().ok()?;
    if filename != format!("{slot:02}.webp")
        || crate::images::validation::validate_slot(slot).is_err()
    {
        return None;
    }
    let metadata = entry.metadata();
    Some(InventoryObject {
        key: key.to_string(),
        namespace: namespace.to_string(),
        resource_id: resource_id.to_string(),
        slot,
        size_bytes: metadata.content_length(),
        etag: metadata.etag().map(str::to_string),
        modified_at: metadata.last_modified().map(|value| value.to_string()),
    })
}

async fn run_db<T>(
    connection: Arc<Mutex<Connection>>,
    task: impl FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = connection
            .lock()
            .map_err(|_| "catalog lock poisoned".to_string())?;
        task(&mut connection).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("catalog database task failed: {error}"))?
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use job_scheduler::{ActivityTracker, JobStatus, SchedulerConfig};
    use std::collections::HashMap;

    fn test_catalog() -> (tempfile::TempDir, StorageCatalog) {
        let directory = tempfile::tempdir().expect("temporary catalog directory");
        let catalog = StorageCatalog::open(directory.path().join("catalog.sqlite3"))
            .expect("catalog should open");
        (directory, catalog)
    }

    fn inventory_object(
        key: &str,
        namespace: &str,
        resource_id: &str,
        slot: u8,
        bytes: u64,
    ) -> InventoryObject {
        InventoryObject {
            key: key.to_string(),
            namespace: namespace.to_string(),
            resource_id: resource_id.to_string(),
            slot,
            size_bytes: bytes,
            etag: None,
            modified_at: None,
        }
    }

    #[tokio::test]
    async fn aggregates_usage_at_global_namespace_resource_and_slot_scopes() {
        let (_directory, catalog) = test_catalog();
        catalog.begin_scan("scan-one").await.unwrap();
        catalog
            .upsert_batch(
                "scan-one",
                vec![
                    inventory_object("products/845/01.webp", "products", "845", 1, 100),
                    inventory_object("products/845/02.webp", "products", "845", 2, 200),
                    inventory_object("products/846/01.webp", "products", "846", 1, 300),
                    inventory_object("avatars/1/01.webp", "avatars", "1", 1, 400),
                ],
            )
            .await
            .unwrap();
        catalog.finish_scan("scan-one").await.unwrap();

        let global = catalog
            .usage(
                UsageFilter {
                    namespace: None,
                    resource_id: None,
                    slot: None,
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!((global.object_count, global.total_bytes), (4, 1000));
        assert!(!global.stale);

        let namespace = catalog
            .usage(
                UsageFilter {
                    namespace: Some("products".into()),
                    resource_id: None,
                    slot: None,
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!((namespace.object_count, namespace.total_bytes), (3, 600));

        let resource = catalog
            .usage(
                UsageFilter {
                    namespace: Some("products".into()),
                    resource_id: Some("845".into()),
                    slot: None,
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!((resource.object_count, resource.total_bytes), (2, 300));

        let slot = catalog
            .usage(
                UsageFilter {
                    namespace: Some("products".into()),
                    resource_id: Some("845".into()),
                    slot: Some(2),
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!((slot.object_count, slot.total_bytes), (1, 200));
    }

    #[tokio::test]
    async fn only_a_successful_completed_scan_removes_stale_objects() {
        let (_directory, catalog) = test_catalog();
        catalog.begin_scan("first").await.unwrap();
        catalog
            .upsert_batch(
                "first",
                vec![inventory_object(
                    "products/845/01.webp",
                    "products",
                    "845",
                    1,
                    100,
                )],
            )
            .await
            .unwrap();
        catalog.finish_scan("first").await.unwrap();

        catalog.begin_scan("interrupted").await.unwrap();
        catalog
            .upsert_batch(
                "interrupted",
                vec![inventory_object(
                    "products/845/01.webp",
                    "products",
                    "845",
                    1,
                    999,
                )],
            )
            .await
            .unwrap();
        catalog.fail_scan("interrupted", "test interruption").await;
        let after_failure = catalog
            .usage(
                UsageFilter {
                    namespace: None,
                    resource_id: None,
                    slot: None,
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!(
            (after_failure.object_count, after_failure.total_bytes),
            (1, 100)
        );
        assert_eq!(after_failure.scan_status, "failed");

        catalog.begin_scan("empty-success").await.unwrap();
        catalog.finish_scan("empty-success").await.unwrap();
        let after_success = catalog
            .usage(
                UsageFilter {
                    namespace: None,
                    resource_id: None,
                    slot: None,
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!(after_success.object_count, 0);
        assert_eq!(after_success.scan_status, "complete");
    }

    #[tokio::test]
    async fn scheduled_reconciliation_indexes_local_filesystem_objects() {
        let directory = tempfile::tempdir().expect("temporary inventory directory");
        let root = directory.path().join("objects");
        let mut options = HashMap::new();
        options.insert("root".to_string(), root.to_string_lossy().into_owned());
        let operator = Operator::via_iter("fs", options).expect("filesystem operator");
        operator
            .write("products/845/01.webp", vec![1_u8; 123])
            .await
            .unwrap();
        operator
            .write("products/845/readme.txt", vec![1_u8; 99])
            .await
            .unwrap();
        operator
            .write("avatars/1/03.webp", vec![2_u8; 77])
            .await
            .unwrap();

        let catalog = StorageCatalog::open(directory.path().join("catalog.sqlite3")).unwrap();
        let scheduler = JobScheduler::open(
            directory.path().join("jobs.sqlite3"),
            SchedulerConfig {
                idle_seconds: 0,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        register_jobs(
            &scheduler,
            catalog.clone(),
            operator,
            3600,
            Arc::new(ActivityTracker::default()),
            0,
        )
        .await
        .unwrap();
        let mut job = NewJob::system(INVENTORY_JOB_QUEUE, INVENTORY_JOB_KIND, Value::Null);
        job.dedupe_key = Some("test-local-reconcile".to_string());
        let job_id = scheduler.store().enqueue(job).await.unwrap();
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let runtime = scheduler.start(Arc::new(ActivityTracker::default()), ready);

        let mut completed = false;
        for _ in 0..100 {
            if scheduler
                .store()
                .get(job_id.clone())
                .await
                .unwrap()
                .is_some_and(|job| job.status == JobStatus::Succeeded)
            {
                completed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        runtime.shutdown(Duration::from_secs(1)).await;
        assert!(completed, "inventory job should complete");
        let result = catalog
            .usage(
                UsageFilter {
                    namespace: None,
                    resource_id: None,
                    slot: None,
                },
                3600,
            )
            .await
            .unwrap();
        assert_eq!((result.object_count, result.total_bytes), (2, 200));
    }
}
