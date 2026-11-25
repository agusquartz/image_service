//! Persistent, eventually consistent storage usage catalog.

use crate::{
    auth::require_auth,
    config::{ProcessingPolicy, ProcessingProfile, resolve_processing_policy},
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
use sha2::Digest;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const INVENTORY_JOB_KIND: &str = "storage.inventory.reconcile";
const INVENTORY_JOB_QUEUE: &str = "storage.inventory";
const INVENTORY_BATCH_SIZE: usize = 250;
const AUDIT_JOB_KIND: &str = "storage.policy.audit";
const AUDIT_JOB_QUEUE: &str = "storage.audit";
const AUDIT_BATCH_SIZE: usize = 10;
const CATALOG_JOB_KIND: &str = "storage.catalog.refresh";
const CATALOG_JOB_QUEUE: &str = "storage.catalog";
const VERIFY_JOB_KIND: &str = "storage.verify.object";
const VERIFY_JOB_QUEUE: &str = "storage.verify";
const MAINTENANCE_JOB_KIND: &str = "storage.maintenance.image";
const MAINTENANCE_JOB_QUEUE: &str = "storage.maintenance";

#[derive(Clone)]
pub(crate) struct StorageCatalog {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InventoryObject {
    pub(crate) key: String,
    pub(crate) namespace: String,
    pub(crate) resource_id: String,
    pub(crate) slot: u8,
    pub(crate) size_bytes: u64,
    pub(crate) etag: Option<String>,
    pub(crate) modified_at: Option<String>,
}

pub(crate) async fn begin_http_mutation(
    state: &AppState,
    key: &crate::images::ImageKey,
    method: &'static str,
) -> Result<Option<String>, ApiError> {
    let Some(catalog) = &state.storage_catalog else {
        return Ok(None);
    };
    let id = uuid::Uuid::new_v4().to_string();
    catalog
        .journal_mutation(
            id.clone(),
            method,
            key.namespace.clone(),
            key.resource_id.clone(),
            Some(key.slot),
        )
        .await
        .map_err(ApiError::Internal)?;
    Ok(Some(id))
}

pub(crate) async fn begin_resource_delete(
    state: &AppState,
    namespace: &str,
    resource_id: &str,
) -> Result<Option<String>, ApiError> {
    let Some(catalog) = &state.storage_catalog else {
        return Ok(None);
    };
    let id = uuid::Uuid::new_v4().to_string();
    catalog
        .journal_mutation(
            id.clone(),
            "DELETE",
            namespace.to_owned(),
            resource_id.to_owned(),
            None,
        )
        .await
        .map_err(ApiError::Internal)?;
    Ok(Some(id))
}

pub(crate) async fn complete_http_mutation(
    state: &AppState,
    operation_id: Option<String>,
    namespace: &str,
    resource_id: &str,
    slot: Option<u8>,
    known_policy: Option<&ProcessingPolicy>,
    write_metadata: Option<&crate::images::StoredMetadata>,
) -> Result<(), ApiError> {
    let (Some(catalog), Some(operation_id)) = (&state.storage_catalog, operation_id) else {
        return Ok(());
    };
    let object = if let Some(slot) = slot {
        let key = crate::images::ImageKey::new(namespace.to_owned(), resource_id.to_owned(), slot)?;
        match state.store.stat(&key).await {
            Ok(metadata) => Some(InventoryObject {
                key: format!("{namespace}/{resource_id}/{slot:02}.webp"),
                namespace: namespace.to_owned(),
                resource_id: resource_id.to_owned(),
                slot,
                size_bytes: metadata.content_length,
                etag: metadata.etag,
                modified_at: metadata.last_modified.map(|value| {
                    value
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string()
                }),
            }),
            Err(ApiError::NotFound(_)) => None,
            Err(error) => {
                queue_mutation_recovery(
                    state,
                    Some(operation_id.clone()),
                    "MUTATION",
                    namespace,
                    resource_id,
                    Some(slot),
                )
                .await;
                return Err(error);
            }
        }
    } else {
        None
    };
    if let Err(error) = catalog
        .apply_mutation(
            operation_id.clone(),
            namespace.to_owned(),
            resource_id.to_owned(),
            slot,
            object.clone(),
        )
        .await
    {
        queue_mutation_recovery(
            state,
            Some(operation_id),
            "MUTATION",
            namespace,
            resource_id,
            slot,
        )
        .await;
        return Err(ApiError::Internal(error));
    }
    if let Some(object) = object {
        let write_is_current = write_metadata.is_some_and(|written| {
            object.size_bytes == written.content_length
                && object.etag == written.etag
                && object.modified_at
                    == written.last_modified.map(|value| {
                        value
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                            .to_string()
                    })
        });
        if let (Some(policy), true) = (known_policy, write_is_current) {
            let snapshot = policy_snapshot(state);
            let fingerprint = policy_fingerprint(&snapshot)?;
            catalog
                .record_verification(
                    object.clone(),
                    fingerprint,
                    "compliant".into(),
                    json!({
                        "format":"webp","animated":false,"width":policy.output_width,
                        "height":policy.output_height,"size_bytes":object.size_bytes,
                        "profile":policy.profile_name,"verified_by":"image_processor"
                    }),
                )
                .await
                .map_err(ApiError::Internal)?;
        } else if let Err(error) = enqueue_verification(state, catalog, object).await {
            tracing::error!(error=?error,"could not queue verification; periodic inventory will retry it");
        }
    }
    Ok(())
}

pub(crate) async fn queue_mutation_recovery(
    state: &AppState,
    operation_id: Option<String>,
    method: &str,
    namespace: &str,
    resource_id: &str,
    slot: Option<u8>,
) {
    let Some(operation_id) = operation_id else {
        return;
    };
    let mut job = NewJob::system(
        CATALOG_JOB_QUEUE,
        CATALOG_JOB_KIND,
        json!({"operation_id":operation_id,"method":method,"namespace":namespace,"resource_id":resource_id,"slot":slot}),
    );
    job.dedupe_key = Some(format!("storage-catalog-recovery:{operation_id}"));
    job.priority = 10;
    job.max_attempts = 10;
    if let Err(error) = state.job_scheduler.store().enqueue(job).await {
        tracing::error!(operation_id, error=%error, "could not enqueue storage journal recovery; pending journal entry will be retried after restart");
    }
}

async fn enqueue_verification(
    state: &AppState,
    catalog: &StorageCatalog,
    object: InventoryObject,
) -> Result<(), ApiError> {
    let snapshot = policy_snapshot(state);
    let fingerprint = policy_fingerprint(&snapshot)?;
    if catalog
        .verification_matches(&object, fingerprint.clone())
        .await
        .map_err(ApiError::Internal)?
    {
        return Ok(());
    }
    let mut job = NewJob::system(
        VERIFY_JOB_QUEUE,
        VERIFY_JOB_KIND,
        serde_json::to_value(&object).map_err(|error| ApiError::Internal(error.to_string()))?,
    );
    job.dedupe_key = Some(format!(
        "storage-verify:{}:{}:{}",
        object.key,
        object.etag.as_deref().unwrap_or("no-etag"),
        object.size_bytes
    ));
    job.policy_snapshot = json!({"policy":snapshot,"fingerprint":fingerprint});
    job.priority = -30;
    job.max_attempts = 5;
    state
        .job_scheduler
        .store()
        .enqueue(job)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(())
}

fn policy_snapshot(state: &AppState) -> AuditPolicySnapshot {
    AuditPolicySnapshot {
        default_max_input_bytes: state.max_source_bytes,
        default_max_output_bytes: state.max_output_bytes,
        default_output_width: state.output_width,
        default_output_height: state.output_height,
        profiles: state.processing_profiles.clone(),
        namespace_profiles: state.namespace_profiles.clone(),
    }
}

fn policy_fingerprint(snapshot: &AuditPolicySnapshot) -> Result<String, ApiError> {
    let mut sorted_profiles = snapshot.profiles.iter().collect::<Vec<_>>();
    sorted_profiles.sort_by_key(|(name, _)| *name);
    let mut sorted_assignments = snapshot.namespace_profiles.iter().collect::<Vec<_>>();
    sorted_assignments.sort_by_key(|(namespace, _)| *namespace);
    let policy_json = serde_json::to_vec(&json!({
        "defaults": [snapshot.default_max_input_bytes, snapshot.default_max_output_bytes, snapshot.default_output_width, snapshot.default_output_height],
        "profiles": sorted_profiles,
        "namespace_profiles": sorted_assignments,
    })).map_err(|error| ApiError::Internal(error.to_string()))?;
    Ok(sha2::Sha256::digest(&policy_json)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditScope {
    pub namespace: Option<String>,
    pub resource_id: Option<String>,
    pub slot: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuditPolicySnapshot {
    default_max_input_bytes: usize,
    default_max_output_bytes: usize,
    default_output_width: u32,
    default_output_height: u32,
    profiles: HashMap<String, ProcessingProfile>,
    namespace_profiles: HashMap<String, String>,
}

impl AuditPolicySnapshot {
    fn policy_for(&self, namespace: &str) -> ProcessingPolicy {
        resolve_processing_policy(
            namespace,
            self.default_max_input_bytes,
            self.default_max_output_bytes,
            self.default_output_width,
            self.default_output_height,
            &self.profiles,
            &self.namespace_profiles,
        )
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AuditCreatedResponse {
    audit_id: String,
    job_id: String,
    status: &'static str,
    inventory_scan_id: String,
    inventory_completed_at_ms: Option<i64>,
    inventory_stale: bool,
    policy_fingerprint: String,
}

#[derive(Debug, Serialize)]
struct AuditSummary {
    audit_id: String,
    job_id: Option<String>,
    status: String,
    scope: AuditScope,
    inventory_scan_id: String,
    inventory_completed_at_ms: Option<i64>,
    inventory_stale: bool,
    policy_fingerprint: String,
    inspected: u64,
    compliant: u64,
    violations: u64,
    changed: u64,
    errors: u64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

#[derive(Debug, Serialize)]
struct AuditFinding {
    object_key: String,
    code: String,
    observed: Value,
}

#[derive(Debug, Serialize)]
struct AuditFindingsPage {
    audit_id: String,
    findings: Vec<AuditFinding>,
    next_cursor: Option<String>,
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
                CREATE TABLE IF NOT EXISTS storage_audits (
                    id TEXT PRIMARY KEY,
                    job_id TEXT,
                    scan_id TEXT NOT NULL,
                    scope_json TEXT NOT NULL,
                    policy_json TEXT NOT NULL,
                    policy_fingerprint TEXT NOT NULL,
                    inventory_completed_ms INTEGER,
                    status TEXT NOT NULL,
                    cursor_key TEXT,
                    inspected INTEGER NOT NULL DEFAULT 0,
                    compliant INTEGER NOT NULL DEFAULT 0,
                    violations INTEGER NOT NULL DEFAULT 0,
                    changed INTEGER NOT NULL DEFAULT 0,
                    errors INTEGER NOT NULL DEFAULT 0,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS storage_audits_scan ON storage_audits(scan_id);
                CREATE TABLE IF NOT EXISTS storage_audit_findings (
                    audit_id TEXT NOT NULL REFERENCES storage_audits(id) ON DELETE CASCADE,
                    object_key TEXT NOT NULL,
                    code TEXT NOT NULL,
                    observed_json TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    PRIMARY KEY(audit_id, object_key, code)
                );
                CREATE INDEX IF NOT EXISTS storage_audit_findings_page
                    ON storage_audit_findings(audit_id, object_key, code);
                CREATE TABLE IF NOT EXISTS storage_mutation_journal (
                    operation_id TEXT PRIMARY KEY,
                    method TEXT NOT NULL,
                    namespace TEXT NOT NULL,
                    resource_id TEXT NOT NULL,
                    slot INTEGER,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at_ms INTEGER NOT NULL,
                    applied_at_ms INTEGER
                );
                CREATE INDEX IF NOT EXISTS storage_mutation_pending
                    ON storage_mutation_journal(status, created_at_ms);
                CREATE TABLE IF NOT EXISTS storage_verification_state (
                    object_key TEXT PRIMARY KEY,
                    namespace TEXT NOT NULL,
                    resource_id TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    etag TEXT,
                    policy_fingerprint TEXT NOT NULL,
                    status TEXT NOT NULL,
                    observed_json TEXT NOT NULL,
                    verified_at_ms INTEGER NOT NULL
                );
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
                    VALUES (1, 'never');
                UPDATE storage_catalog_state SET last_completed_scan_id='live-incremental'
                    WHERE id=1 AND last_completed_scan_id IS NULL;",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) async fn journal_mutation(
        &self,
        operation_id: String,
        method: &'static str,
        namespace: String,
        resource_id: String,
        slot: Option<u8>,
    ) -> Result<(), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            connection.execute(
                "INSERT OR IGNORE INTO storage_mutation_journal(operation_id,method,namespace,resource_id,slot,status,created_at_ms) VALUES (?1,?2,?3,?4,?5,'pending',?6)",
                params![operation_id, method, namespace, resource_id, slot, now_ms()],
            )?;
            Ok(())
        }).await
    }

    async fn pending_mutations(
        &self,
    ) -> Result<Vec<(String, String, String, String, Option<u8>)>, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let mut statement = connection.prepare("SELECT operation_id,method,namespace,resource_id,slot FROM storage_mutation_journal WHERE status='pending' ORDER BY created_at_ms LIMIT 500")?;
            statement.query_map([], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?.collect::<Result<Vec<_>,_>>()
        }).await
    }

    pub(crate) async fn apply_mutation(
        &self,
        operation_id: String,
        namespace: String,
        resource_id: String,
        slot: Option<u8>,
        object: Option<InventoryObject>,
    ) -> Result<(), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let scan_ids = {
                let mut statement = transaction.prepare("SELECT last_completed_scan_id FROM storage_catalog_state WHERE id=1 UNION SELECT last_scan_id FROM storage_catalog_state WHERE id=1 AND scan_status='running'")?;
                statement.query_map([], |row| row.get::<_, Option<String>>(0))?.collect::<Result<Vec<_>,_>>()?
            };
            for scan_id in scan_ids.into_iter().flatten() {
                if let Some(object) = &object {
                    transaction.execute(
                        "INSERT INTO storage_objects(scan_id,object_key,namespace,resource_id,slot,size_bytes,etag,modified_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(scan_id,object_key) DO UPDATE SET size_bytes=excluded.size_bytes,etag=excluded.etag,modified_at=excluded.modified_at",
                        params![scan_id,object.key,object.namespace,object.resource_id,object.slot,object.size_bytes,object.etag,object.modified_at],
                    )?;
                } else if let Some(slot) = slot {
                    transaction.execute("DELETE FROM storage_objects WHERE scan_id=?1 AND namespace=?2 AND resource_id=?3 AND slot=?4", params![scan_id,namespace,resource_id,slot])?;
                } else {
                    transaction.execute("DELETE FROM storage_objects WHERE scan_id=?1 AND namespace=?2 AND resource_id=?3", params![scan_id,namespace,resource_id])?;
                }
            }
            if let Some(object) = object {
                transaction.execute("DELETE FROM storage_verification_state WHERE object_key=?1", [&object.key])?;
            } else if let Some(slot) = slot {
                transaction.execute("DELETE FROM storage_verification_state WHERE object_key=?1", [format!("{namespace}/{resource_id}/{slot:02}.webp")])?;
            } else {
                transaction.execute("DELETE FROM storage_verification_state WHERE namespace=?1 AND resource_id=?2", params![namespace,resource_id])?;
            }
            transaction.execute("UPDATE storage_mutation_journal SET status='applied',applied_at_ms=?2 WHERE operation_id=?1", params![operation_id,now_ms()])?;
            transaction.commit()?;
            Ok(())
        }).await
    }

    async fn record_verification(
        &self,
        object: InventoryObject,
        fingerprint: String,
        status: String,
        observed: Value,
    ) -> Result<(), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            connection.execute("INSERT INTO storage_verification_state(object_key,namespace,resource_id,size_bytes,etag,policy_fingerprint,status,observed_json,verified_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(object_key) DO UPDATE SET namespace=excluded.namespace,resource_id=excluded.resource_id,size_bytes=excluded.size_bytes,etag=excluded.etag,policy_fingerprint=excluded.policy_fingerprint,status=excluded.status,observed_json=excluded.observed_json,verified_at_ms=excluded.verified_at_ms", params![object.key,object.namespace,object.resource_id,object.size_bytes,object.etag,fingerprint,status,observed.to_string(),now_ms()])?;
            Ok(())
        }).await
    }

    async fn verification_matches(
        &self,
        object: &InventoryObject,
        fingerprint: String,
    ) -> Result<bool, String> {
        let connection = self.connection.clone();
        let key = object.key.clone();
        let size = object.size_bytes;
        let etag = object.etag.clone();
        run_db(connection, move |connection| {
            let matches: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM storage_verification_state WHERE object_key=?1 AND size_bytes=?2 AND etag IS ?3 AND policy_fingerprint=?4)", params![key,size,etag,fingerprint], |row| row.get(0))?;
            Ok(matches)
        }).await
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
            transaction.execute(
                "DELETE FROM storage_objects WHERE scan_id <> ?1 AND scan_id NOT IN (SELECT scan_id FROM storage_audits WHERE status IN ('queued','running','retry_wait'))",
                [&scan_id],
            )?;
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

    async fn create_audit(
        &self,
        id: String,
        scope: AuditScope,
        snapshot: AuditPolicySnapshot,
        fingerprint: String,
    ) -> Result<(String, Option<i64>), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let (scan_id, completed_ms): (Option<String>, Option<i64>) = connection.query_row(
                "SELECT last_completed_scan_id, last_scan_completed_ms FROM storage_catalog_state WHERE id=1",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if completed_ms.is_none() {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            let scan_id = scan_id.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let now = now_ms();
            connection.execute(
                "INSERT INTO storage_audits(id, scan_id, scope_json, policy_json, policy_fingerprint, inventory_completed_ms, status, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?7)",
                params![id, scan_id, serde_json::to_string(&scope).map_err(|_| rusqlite::Error::InvalidQuery)?, serde_json::to_string(&snapshot).map_err(|_| rusqlite::Error::InvalidQuery)?, fingerprint, completed_ms, now],
            )?;
            Ok((scan_id, completed_ms))
        }).await
    }

    async fn attach_audit_job(&self, audit_id: String, job_id: String) -> Result<(), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            connection.execute(
                "UPDATE storage_audits SET job_id=?2, updated_at_ms=?3 WHERE id=?1",
                params![audit_id, job_id, now_ms()],
            )?;
            Ok(())
        })
        .await
    }

    async fn orphaned_audits(&self) -> Result<Vec<(String, String, String)>, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let mut statement = connection.prepare("SELECT id,scan_id,policy_fingerprint FROM storage_audits WHERE status='queued' AND job_id IS NULL ORDER BY created_at_ms")?;
            statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<Result<Vec<_>,_>>()
        }).await
    }

    async fn audit_payload(
        &self,
        audit_id: String,
    ) -> Result<(String, AuditScope, AuditPolicySnapshot, Option<String>), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            connection.query_row(
                "SELECT scan_id, scope_json, policy_json, cursor_key FROM storage_audits WHERE id=?1",
                [audit_id],
                |row| {
                    let scope: String = row.get(1)?;
                    let policy: String = row.get(2)?;
                    Ok((row.get(0)?, serde_json::from_str(&scope).map_err(|_| rusqlite::Error::InvalidQuery)?, serde_json::from_str(&policy).map_err(|_| rusqlite::Error::InvalidQuery)?, row.get(3)?))
                },
            )
        })
        .await
    }

    async fn audit_objects(
        &self,
        scan_id: String,
        scope: AuditScope,
        after: Option<String>,
    ) -> Result<Vec<InventoryObject>, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let mut statement = connection.prepare(
                "SELECT object_key, namespace, resource_id, slot, size_bytes, etag, modified_at FROM storage_objects WHERE scan_id=?1 AND object_key>?2 AND (?3 IS NULL OR namespace=?3) AND (?4 IS NULL OR resource_id=?4) AND (?5 IS NULL OR slot=?5) ORDER BY object_key LIMIT ?6"
            )?;
            let rows = statement.query_map(params![scan_id, after.unwrap_or_default(), scope.namespace, scope.resource_id, scope.slot, AUDIT_BATCH_SIZE as i64], |row| {
                Ok(InventoryObject { key: row.get(0)?, namespace: row.get(1)?, resource_id: row.get(2)?, slot: row.get(3)?, size_bytes: row.get(4)?, etag: row.get(5)?, modified_at: row.get(6)? })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        }).await
    }

    async fn scan_objects(&self, scan_id: String) -> Result<Vec<InventoryObject>, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let mut statement = connection.prepare("SELECT object_key,namespace,resource_id,slot,size_bytes,etag,modified_at FROM storage_objects WHERE scan_id=?1 ORDER BY object_key")?;
            statement.query_map([scan_id], |row| Ok(InventoryObject { key:row.get(0)?,namespace:row.get(1)?,resource_id:row.get(2)?,slot:row.get(3)?,size_bytes:row.get(4)?,etag:row.get(5)?,modified_at:row.get(6)? }))?.collect::<Result<Vec<_>,_>>()
        }).await
    }

    async fn record_audit_result(
        &self,
        audit_id: String,
        object: InventoryObject,
        result: AuditObjectResult,
    ) -> Result<(), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let (inspected, compliant, violations, changed, errors) = match result {
                AuditObjectResult::Compliant(observed) => {
                    let _ = observed;
                    (1, 1, 0, 0, 0)
                }
                AuditObjectResult::Finding(code, observed) => {
                    transaction.execute("INSERT OR REPLACE INTO storage_audit_findings(audit_id, object_key, code, observed_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)", params![audit_id, object.key, code, observed.to_string(), now_ms()])?;
                    let inspection_error = i64::from(code == "invalid_or_unsupported_image");
                    (1, 0, 1, 0, inspection_error)
                }
                AuditObjectResult::Uninspectable(code, observed) => {
                    transaction.execute("INSERT OR REPLACE INTO storage_audit_findings(audit_id, object_key, code, observed_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)", params![audit_id, object.key, code, observed.to_string(), now_ms()])?;
                    (0, 0, 0, 0, 1)
                }
                AuditObjectResult::Changed(observed) => {
                    transaction.execute("INSERT OR REPLACE INTO storage_audit_findings(audit_id, object_key, code, observed_json, created_at_ms) VALUES (?1, ?2, 'object_changed', ?3, ?4)", params![audit_id, object.key, observed.to_string(), now_ms()])?;
                    (0, 0, 0, 1, 0)
                }
            };
            transaction.execute(
                "UPDATE storage_audits SET cursor_key=?2, inspected=inspected+?3, compliant=compliant+?4, violations=violations+?5, changed=changed+?6, errors=errors+?7, status='running', updated_at_ms=?8 WHERE id=?1",
                params![audit_id, object.key, inspected, compliant, violations, changed, errors, now_ms()],
            )?;
            transaction.commit()?;
            Ok(())
        }).await
    }

    async fn set_audit_status(&self, audit_id: String, status: &'static str) -> Result<(), String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            connection.execute(
                "UPDATE storage_audits SET status=?2, updated_at_ms=?3 WHERE id=?1",
                params![audit_id, status, now_ms()],
            )?;
            Ok(())
        })
        .await
    }

    async fn audit_summary(
        &self,
        audit_id: String,
        stale_after_seconds: u64,
    ) -> Result<AuditSummary, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let row = connection.query_row(
                "SELECT a.id,a.job_id,a.scan_id,a.scope_json,a.policy_fingerprint,a.status,a.inspected,a.compliant,a.violations,a.changed,a.errors,a.created_at_ms,a.updated_at_ms,a.inventory_completed_ms FROM storage_audits a WHERE a.id=?1",
                [audit_id], |row| {
                    let scope: String = row.get(3)?;
                    Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,String>(2)?,serde_json::from_str::<AuditScope>(&scope).map_err(|_|rusqlite::Error::InvalidQuery)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,i64>(6)?,row.get::<_,i64>(7)?,row.get::<_,i64>(8)?,row.get::<_,i64>(9)?,row.get::<_,i64>(10)?,row.get::<_,i64>(11)?,row.get::<_,i64>(12)?,row.get::<_,Option<i64>>(13)?))
                }
            )?;
            let (id,job_id,scan_id,scope,fingerprint,status,inspected,compliant,violations,changed,errors,created,updated,completed)=row;
            let stale = completed.is_none_or(|at| now_ms().saturating_sub(at).max(0) as u64 > stale_after_seconds.saturating_mul(1000));
            Ok(AuditSummary { audit_id:id, job_id, status, scope, inventory_scan_id:scan_id, inventory_completed_at_ms:completed, inventory_stale:stale, policy_fingerprint:fingerprint, inspected:inspected.max(0) as u64, compliant:compliant.max(0) as u64, violations:violations.max(0) as u64, changed:changed.max(0) as u64, errors:errors.max(0) as u64, created_at_ms:created, updated_at_ms:updated })
        }).await
    }

    async fn audit_findings(
        &self,
        audit_id: String,
        after: Option<(String, String)>,
        limit: usize,
    ) -> Result<AuditFindingsPage, String> {
        let connection = self.connection.clone();
        run_db(connection, move |connection| {
            let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM storage_audits WHERE id=?1)", [&audit_id], |row| row.get(0))?;
            if !exists { return Err(rusqlite::Error::QueryReturnedNoRows); }
            let (after_key, after_code) = after.unwrap_or_default();
            let mut statement = connection.prepare("SELECT object_key,code,observed_json FROM storage_audit_findings WHERE audit_id=?1 AND (object_key>?2 OR (object_key=?2 AND code>?3)) ORDER BY object_key,code LIMIT ?4")?;
            let rows = statement.query_map(params![audit_id, after_key, after_code, (limit + 1) as i64], |row| {
                let observed: String = row.get(2)?;
                Ok(AuditFinding { object_key: row.get(0)?, code: row.get(1)?, observed: serde_json::from_str(&observed).unwrap_or(Value::Null) })
            })?.collect::<Result<Vec<_>,_>>()?;
            let mut findings = rows;
            let next_cursor = if findings.len() > limit {
                findings.pop();
                findings
                    .last()
                    .map(|item| encode_findings_cursor(&item.object_key, &item.code))
            } else {
                None
            };
            Ok(AuditFindingsPage { audit_id, findings, next_cursor })
        }).await
    }
}

enum AuditObjectResult {
    Compliant(Value),
    Finding(String, Value),
    Uninspectable(String, Value),
    Changed(Value),
}

fn encode_findings_cursor(object_key: &str, code: &str) -> String {
    format!("{object_key}\n{code}")
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_findings_cursor(cursor: &str) -> Result<(String, String), ApiError> {
    if cursor.is_empty() {
        return Err(ApiError::BadRequest("Invalid findings cursor".into()));
    }
    let bytes = cursor
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let [high, low] = pair else {
                return Err(ApiError::BadRequest("Invalid findings cursor".into()));
            };
            let pair = [*high, *low];
            let pair = std::str::from_utf8(&pair)
                .map_err(|_| ApiError::BadRequest("Invalid findings cursor".into()))?;
            u8::from_str_radix(pair, 16)
                .map_err(|_| ApiError::BadRequest("Invalid findings cursor".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let decoded = String::from_utf8(bytes)
        .map_err(|_| ApiError::BadRequest("Invalid findings cursor".into()))?;
    let (key, code) = decoded
        .split_once('\n')
        .ok_or_else(|| ApiError::BadRequest("Invalid findings cursor".into()))?;
    Ok((key.to_string(), code.to_string()))
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

pub(crate) async fn start_policy_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(scope): Json<AuditScope>,
) -> Result<Response, ApiError> {
    require_usage_auth(&headers, &state)?;
    validate_audit_scope(&scope)?;
    let catalog = state
        .storage_catalog
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("Storage usage reporting is disabled".into()))?;
    let snapshot = AuditPolicySnapshot {
        default_max_input_bytes: state.max_source_bytes,
        default_max_output_bytes: state.max_output_bytes,
        default_output_width: state.output_width,
        default_output_height: state.output_height,
        profiles: state.processing_profiles.clone(),
        namespace_profiles: state.namespace_profiles.clone(),
    };
    let mut sorted_profiles = snapshot.profiles.iter().collect::<Vec<_>>();
    sorted_profiles.sort_by_key(|(name, _)| *name);
    let mut sorted_assignments = snapshot.namespace_profiles.iter().collect::<Vec<_>>();
    sorted_assignments.sort_by_key(|(namespace, _)| *namespace);
    let policy_json = serde_json::to_vec(&json!({
        "defaults": [snapshot.default_max_input_bytes, snapshot.default_max_output_bytes, snapshot.default_output_width, snapshot.default_output_height],
        "profiles": sorted_profiles,
        "namespace_profiles": sorted_assignments,
    })).map_err(|error| ApiError::Internal(error.to_string()))?;
    let fingerprint = sha2::Sha256::digest(&policy_json)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let audit_id = uuid::Uuid::new_v4().to_string();
    let (scan_id, completed_at) = catalog
        .create_audit(audit_id.clone(), scope, snapshot, fingerprint.clone())
        .await
        .map_err(|error| {
            if error.contains("Query returned no rows") {
                ApiError::Conflict("Run a storage reconciliation before starting an audit".into())
            } else {
                ApiError::Internal(error)
            }
        })?;
    let mut job = NewJob::system(
        AUDIT_JOB_QUEUE,
        AUDIT_JOB_KIND,
        json!({"audit_id": audit_id}),
    );
    job.dedupe_key = Some(format!("storage-policy-audit:{audit_id}"));
    job.policy_snapshot = json!({"inventory_scan_id":scan_id,"policy_fingerprint":fingerprint});
    job.priority = -20;
    job.max_attempts = 5;
    let job_id = match state.job_scheduler.store().enqueue(job).await {
        Ok(id) => id,
        Err(error) => {
            let _ = catalog.set_audit_status(audit_id, "failed").await;
            return Err(ApiError::Internal(error.to_string()));
        }
    };
    catalog
        .attach_audit_job(audit_id.clone(), job_id.clone())
        .await
        .map_err(ApiError::Internal)?;
    let stale = completed_at.is_none_or(|at| {
        now_ms().saturating_sub(at).max(0) as u64
            > state
                .storage_inventory_interval_seconds
                .saturating_mul(2)
                .saturating_mul(1000)
    });
    let mut response = (
        StatusCode::ACCEPTED,
        Json(AuditCreatedResponse {
            audit_id,
            job_id,
            status: "queued",
            inventory_scan_id: scan_id,
            inventory_completed_at_ms: completed_at,
            inventory_stale: stale,
            policy_fingerprint: fingerprint,
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

pub(crate) async fn get_policy_audit(
    State(state): State<AppState>,
    axum::extract::Path(audit_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_usage_auth(&headers, &state)?;
    let catalog = state
        .storage_catalog
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("Storage usage reporting is disabled".into()))?;
    let summary = catalog
        .audit_summary(
            audit_id,
            state.storage_inventory_interval_seconds.saturating_mul(2),
        )
        .await
        .map_err(|error| {
            if error.contains("Query returned no rows") {
                ApiError::NotFound("Storage audit not found".into())
            } else {
                ApiError::Internal(error)
            }
        })?;
    let mut response = Json(summary).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

#[derive(Debug, Deserialize)]
pub(crate) struct FindingsQuery {
    after: Option<String>,
    limit: Option<usize>,
}

pub(crate) async fn get_policy_audit_findings(
    State(state): State<AppState>,
    axum::extract::Path(audit_id): axum::extract::Path<String>,
    Query(query): Query<FindingsQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_usage_auth(&headers, &state)?;
    let catalog = state
        .storage_catalog
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("Storage usage reporting is disabled".into()))?;
    let cursor = query
        .after
        .map(|value| decode_findings_cursor(&value))
        .transpose()?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let page = catalog
        .audit_findings(audit_id, cursor, limit)
        .await
        .map_err(|error| {
            if error.contains("Query returned no rows") {
                ApiError::NotFound("Storage audit not found".into())
            } else {
                ApiError::Internal(error)
            }
        })?;
    let mut response = Json(page).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

fn validate_audit_scope(scope: &AuditScope) -> Result<(), ApiError> {
    if let Some(namespace) = &scope.namespace {
        validate_namespace(namespace)?;
    }
    if let Some(resource) = &scope.resource_id {
        if scope.namespace.is_none() {
            return Err(ApiError::BadRequest(
                "namespace is required with resource_id".into(),
            ));
        }
        validate_resource_id(resource)?;
    }
    if let Some(slot) = scope.slot {
        if scope.resource_id.is_none() {
            return Err(ApiError::BadRequest(
                "resource_id is required with slot".into(),
            ));
        }
        crate::images::validation::validate_slot(slot)?;
    }
    Ok(())
}

fn require_usage_auth(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    require_auth(
        headers,
        crate::config::AuthRequirement::Required,
        state.api_key.as_deref(),
    )
}

pub(crate) async fn register_jobs(state: &AppState) -> Result<(), String> {
    let scheduler = &state.job_scheduler;
    let catalog = state
        .storage_catalog
        .clone()
        .ok_or_else(|| "storage catalog is disabled".to_string())?;
    register_inventory_handler(
        scheduler,
        catalog.clone(),
        state.storage_operator.clone(),
        state.activity.clone(),
        state.background_idle_seconds,
        Some(state.clone()),
    )
    .await?;
    scheduler
        .register_handler(
            CATALOG_JOB_KIND,
            Arc::new(CatalogRecoveryHandler {
                state: state.clone(),
                catalog: catalog.clone(),
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    scheduler
        .register_handler(
            VERIFY_JOB_KIND,
            Arc::new(VerificationHandler {
                state: state.clone(),
                catalog: catalog.clone(),
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    scheduler
        .register_handler(
            MAINTENANCE_JOB_KIND,
            Arc::new(MaintenanceHandler {
                state: state.clone(),
                catalog: catalog.clone(),
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    for (operation_id, method, namespace, resource_id, slot) in catalog.pending_mutations().await? {
        let mut job = NewJob::system(
            CATALOG_JOB_QUEUE,
            CATALOG_JOB_KIND,
            json!({"operation_id":operation_id,"method":method,"namespace":namespace,"resource_id":resource_id,"slot":slot}),
        );
        job.dedupe_key = Some(format!("storage-catalog-recovery:{operation_id}"));
        job.priority = 10;
        job.max_attempts = 10;
        scheduler
            .store()
            .enqueue(job)
            .await
            .map_err(|error| error.to_string())?;
    }
    scheduler
        .register_handler(
            AUDIT_JOB_KIND,
            Arc::new(PolicyAuditHandler {
                state: state.clone(),
                catalog: catalog.clone(),
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    for (audit_id, scan_id, fingerprint) in catalog.orphaned_audits().await? {
        let mut job = NewJob::system(
            AUDIT_JOB_QUEUE,
            AUDIT_JOB_KIND,
            json!({"audit_id": audit_id}),
        );
        job.dedupe_key = Some(format!("storage-policy-audit:{audit_id}"));
        job.policy_snapshot = json!({"inventory_scan_id":scan_id,"policy_fingerprint":fingerprint});
        job.priority = -20;
        job.max_attempts = 5;
        let job_id = scheduler
            .store()
            .enqueue(job)
            .await
            .map_err(|error| error.to_string())?;
        catalog.attach_audit_job(audit_id, job_id).await?;
    }
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
            interval: Duration::from_secs(state.storage_inventory_interval_seconds),
            run_immediately: true,
        })
        .await
        .map_err(|error| error.to_string())
}

async fn register_inventory_handler(
    scheduler: &JobScheduler,
    catalog: StorageCatalog,
    operator: Operator,
    activity: Arc<ActivityTracker>,
    idle_seconds: u64,
    state: Option<AppState>,
) -> Result<(), String> {
    scheduler
        .register_handler(
            INVENTORY_JOB_KIND,
            Arc::new(InventoryHandler {
                catalog,
                operator,
                activity,
                idle_seconds,
                state,
            }),
        )
        .await
        .map_err(|error| error.to_string())
}

struct InventoryHandler {
    catalog: StorageCatalog,
    operator: Operator,
    activity: Arc<ActivityTracker>,
    idle_seconds: u64,
    state: Option<AppState>,
}

struct CatalogRecoveryHandler {
    state: AppState,
    catalog: StorageCatalog,
}
struct VerificationHandler {
    state: AppState,
    catalog: StorageCatalog,
}
struct MaintenanceHandler {
    state: AppState,
    catalog: StorageCatalog,
}

impl JobHandler for CatalogRecoveryHandler {
    fn execute<'a>(&'a self, context: JobContext) -> JobFuture<'a> {
        Box::pin(async move {
            let payload = &context.job.payload;
            let operation_id = payload
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let namespace = payload
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let resource_id = payload
                .get("resource_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let slot = payload
                .get("slot")
                .and_then(Value::as_u64)
                .map(|value| value as u8);
            let result = async {
                let object = if let Some(slot) = slot {
                    let key =
                        crate::images::ImageKey::new(namespace.clone(), resource_id.clone(), slot)
                            .map_err(|error| format!("invalid journal key: {error:?}"))?;
                    match self.state.store.stat(&key).await {
                        Ok(metadata) => Some(InventoryObject {
                            key: format!("{namespace}/{resource_id}/{slot:02}.webp"),
                            namespace: namespace.clone(),
                            resource_id: resource_id.clone(),
                            slot,
                            size_bytes: metadata.content_length,
                            etag: metadata.etag,
                            modified_at: metadata.last_modified.map(|value| {
                                value
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                                    .to_string()
                            }),
                        }),
                        Err(ApiError::NotFound(_)) => None,
                        Err(error) => return Err(format!("storage stat failed: {error:?}")),
                    }
                } else {
                    None
                };
                self.catalog
                    .apply_mutation(operation_id, namespace, resource_id, slot, object.clone())
                    .await?;
                if let Some(object) = object {
                    enqueue_verification(&self.state, &self.catalog, object)
                        .await
                        .map_err(|error| format!("verification enqueue failed: {error:?}"))?;
                }
                Ok::<(), String>(())
            }
            .await;
            match result {
                Ok(()) => JobOutcome::Succeeded(json!({"recovered":true})),
                Err(message) => JobOutcome::Failed {
                    message,
                    retryable: true,
                },
            }
        })
    }
}

impl JobHandler for VerificationHandler {
    fn execute<'a>(&'a self, mut context: JobContext) -> JobFuture<'a> {
        Box::pin(async move {
            let result = self.run(&mut context).await;
            match result {
                Ok(result) => JobOutcome::Succeeded(result),
                Err(message) => JobOutcome::Failed {
                    message,
                    retryable: true,
                },
            }
        })
    }
}

impl VerificationHandler {
    async fn run(&self, context: &mut JobContext) -> Result<Value, String> {
        wait_until_idle(&self.state, context).await?;
        let object: InventoryObject = serde_json::from_value(context.job.payload.clone())
            .map_err(|error| error.to_string())?;
        let snapshot: AuditPolicySnapshot = serde_json::from_value(
            context
                .job
                .policy_snapshot
                .get("policy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|error| format!("invalid verification policy snapshot: {error}"))?;
        let fingerprint = context
            .job
            .policy_snapshot
            .get("fingerprint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if self
            .catalog
            .verification_matches(&object, fingerprint.clone())
            .await?
        {
            return Ok(json!({"skipped":"already_verified"}));
        }
        let auditor = PolicyAuditHandler {
            state: self.state.clone(),
            catalog: self.catalog.clone(),
        };
        let result = auditor.inspect_object(&object, &snapshot).await?;
        let (status, observed, needs_maintenance) = match result {
            AuditObjectResult::Compliant(observed) => ("compliant", observed, false),
            AuditObjectResult::Finding(code, observed) => {
                let repairable = !code.contains("invalid_or_unsupported_image");
                ("violation", observed, repairable)
            }
            AuditObjectResult::Uninspectable(code, observed) => (
                "uninspectable",
                json!({"code":code,"details":observed}),
                false,
            ),
            AuditObjectResult::Changed(observed) => ("changed", observed, false),
        };
        self.catalog
            .record_verification(
                object.clone(),
                fingerprint.clone(),
                status.into(),
                observed.clone(),
            )
            .await?;
        if needs_maintenance {
            let mut job = NewJob::system(
                MAINTENANCE_JOB_QUEUE,
                MAINTENANCE_JOB_KIND,
                serde_json::to_value(&object).map_err(|error| error.to_string())?,
            );
            job.dedupe_key = Some(format!(
                "storage-maintenance:{}:{}",
                object.key, fingerprint
            ));
            job.policy_snapshot = json!({"policy":snapshot,"fingerprint":fingerprint,"expected_etag":object.etag,"expected_size":object.size_bytes});
            job.priority = -40;
            job.max_attempts = 3;
            self.state
                .job_scheduler
                .store()
                .enqueue(job)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(json!({"object_key":object.key,"status":status,"maintenance_queued":needs_maintenance}))
    }
}

impl JobHandler for MaintenanceHandler {
    fn execute<'a>(&'a self, mut context: JobContext) -> JobFuture<'a> {
        Box::pin(async move {
            match self.run(&mut context).await {
                Ok(value) => JobOutcome::Succeeded(value),
                Err(message) => JobOutcome::Failed {
                    message,
                    retryable: true,
                },
            }
        })
    }
}

impl MaintenanceHandler {
    async fn run(&self, context: &mut JobContext) -> Result<Value, String> {
        wait_until_idle(&self.state, context).await?;
        let object: InventoryObject = serde_json::from_value(context.job.payload.clone())
            .map_err(|error| error.to_string())?;
        let captured_policy: AuditPolicySnapshot = serde_json::from_value(
            context
                .job
                .policy_snapshot
                .get("policy")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|error| format!("invalid maintenance policy: {error}"))?;
        let current_snapshot = policy_snapshot(&self.state);
        if policy_fingerprint(&current_snapshot)
            .map_err(|error| format!("could not fingerprint current policy: {error:?}"))?
            != context
                .job
                .policy_snapshot
                .get("fingerprint")
                .and_then(Value::as_str)
                .unwrap_or_default()
        {
            enqueue_verification(&self.state, &self.catalog, object.clone())
                .await
                .map_err(|error| {
                    format!("could not queue verification for changed policy: {error:?}")
                })?;
            return Ok(json!({"status":"policy_changed"}));
        }
        let policy = captured_policy.policy_for(&object.namespace);
        if object.size_bytes > self.state.max_request_source_bytes as u64 {
            return Err(format!(
                "{} exceeds the configured inspection/input limit",
                object.key
            ));
        }
        let key = crate::images::ImageKey::new(
            object.namespace.clone(),
            object.resource_id.clone(),
            object.slot,
        )
        .map_err(|error| format!("invalid object key: {error:?}"))?;
        let admission = self
            .state
            .admit_background_read(object.size_bytes as usize)
            .map_err(|error| format!("maintenance memory admission unavailable: {error:?}"))?;
        let (metadata, bytes) = match self.state.store.read_snapshot(&key).await {
            Ok(value) => value,
            Err(ApiError::NotFound(_)) => return Ok(json!({"status":"object_removed"})),
            Err(error) => return Err(format!("could not read object: {error:?}")),
        };
        if metadata.content_length != object.size_bytes
            || (object.etag.is_some() && metadata.etag != object.etag)
        {
            return Ok(json!({"status":"superseded"}));
        }
        let queue_started = std::time::Instant::now();
        let memory_permit = self
            .state
            .try_admit_image_processing(policy.output_width, policy.output_height)
            .map_err(|_| {
                "image processing memory budget is busy; retry maintenance later".to_string()
            })?;
        let permit = self
            .state
            .image_jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                "image processing capacity is busy; retry maintenance later".to_string()
            })?;
        let queue_wait_us = queue_started.elapsed().as_micros() as u64;
        let processor = self.state.image_processor.clone();
        let request = processor_contract::ProcessingRequest {
            source: axum::body::Bytes::from(bytes),
            declared_media_type: None,
            max_output_bytes: policy.max_output_bytes,
            output_width: policy.output_width,
            output_height: policy.output_height,
            max_decode_dimension: self.state.max_decode_dimension,
            max_decode_allocation_bytes: self.state.max_decode_allocation_bytes,
            queue_wait_us,
        };
        let output = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _memory_permit = memory_permit;
            processor.process(request)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        drop(admission);
        let expected_etag = context
            .job
            .policy_snapshot
            .get("expected_etag")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if expected_etag != metadata.etag {
            return Ok(json!({"status":"superseded"}));
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        self.catalog
            .journal_mutation(
                operation_id.clone(),
                "MAINTENANCE",
                object.namespace.clone(),
                object.resource_id.clone(),
                Some(object.slot),
            )
            .await?;
        let written = match self
            .state
            .store
            .replace_if_unchanged(&key, &metadata, output.bytes)
            .await
        {
            Ok(written) => written,
            Err(error) => {
                queue_mutation_recovery(
                    &self.state,
                    Some(operation_id),
                    "MAINTENANCE",
                    &object.namespace,
                    &object.resource_id,
                    Some(object.slot),
                )
                .await;
                return Err(format!("conditional maintenance write failed: {error:?}"));
            }
        };
        complete_http_mutation(
            &self.state,
            Some(operation_id),
            &object.namespace,
            &object.resource_id,
            Some(object.slot),
            None,
            None,
        )
        .await
        .map_err(|error| format!("post-maintenance catalog update failed: {error:?}"))?;
        if !written {
            return Ok(json!({"status":"superseded"}));
        }
        Ok(json!({"status":"reprocessed","object_key":object.key}))
    }
}

async fn wait_until_idle(state: &AppState, context: &JobContext) -> Result<(), String> {
    let quiet = Duration::from_secs(state.background_idle_seconds);
    while !state.activity.is_idle_for(quiet) {
        if context.cancellation_requested() {
            return Err("Background job cancelled during idle wait".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

struct PolicyAuditHandler {
    state: AppState,
    catalog: StorageCatalog,
}

impl JobHandler for PolicyAuditHandler {
    fn execute<'a>(&'a self, mut context: JobContext) -> JobFuture<'a> {
        Box::pin(async move {
            let audit_id = context
                .job
                .payload
                .get("audit_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if audit_id.is_empty() {
                return JobOutcome::Failed {
                    message: "Missing audit_id".into(),
                    retryable: false,
                };
            }
            match self.run_audit(&mut context, &audit_id).await {
                Ok(()) => JobOutcome::Succeeded(json!({"audit_id":audit_id,"status":"complete"})),
                Err(error) => {
                    let status = if context.job.attempts >= context.job.max_attempts {
                        "failed"
                    } else {
                        "retry_wait"
                    };
                    let _ = self.catalog.set_audit_status(audit_id, status).await;
                    JobOutcome::Failed {
                        message: error,
                        retryable: true,
                    }
                }
            }
        })
    }
}

impl PolicyAuditHandler {
    async fn run_audit(&self, context: &mut JobContext, audit_id: &str) -> Result<(), String> {
        let (scan_id, scope, snapshot, mut cursor) =
            self.catalog.audit_payload(audit_id.to_string()).await?;
        self.catalog
            .set_audit_status(audit_id.to_string(), "running")
            .await?;
        loop {
            if context.cancellation_requested() {
                self.catalog
                    .set_audit_status(audit_id.to_string(), "cancelled")
                    .await?;
                return Ok(());
            }
            let batch = self
                .catalog
                .audit_objects(scan_id.clone(), scope.clone(), cursor.clone())
                .await?;
            if batch.is_empty() {
                break;
            }
            for object in batch.iter().cloned() {
                if context.cancellation_requested() {
                    self.catalog
                        .set_audit_status(audit_id.to_string(), "cancelled")
                        .await?;
                    return Ok(());
                }
                let result = self.inspect_object(&object, &snapshot).await?;
                self.catalog
                    .record_audit_result(audit_id.to_string(), object.clone(), result)
                    .await?;
                cursor = Some(object.key);
                context
                    .checkpoint(
                        json!({"audit_id":audit_id,"last_object_key":cursor,"scan_id":scan_id}),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
            if batch.len() < AUDIT_BATCH_SIZE {
                break;
            }
            while !self
                .state
                .activity
                .is_idle_for(Duration::from_secs(self.state.background_idle_seconds))
            {
                if context.cancellation_requested() {
                    self.catalog
                        .set_audit_status(audit_id.to_string(), "cancelled")
                        .await?;
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        self.catalog
            .set_audit_status(audit_id.to_string(), "complete")
            .await
    }

    async fn inspect_object(
        &self,
        object: &InventoryObject,
        snapshot: &AuditPolicySnapshot,
    ) -> Result<AuditObjectResult, String> {
        let policy = snapshot.policy_for(&object.namespace);
        if object.size_bytes > self.state.max_request_source_bytes as u64 {
            return Ok(AuditObjectResult::Uninspectable(
                "too_large_to_inspect".into(),
                json!({"size_bytes":object.size_bytes,"inspection_limit_bytes":self.state.max_request_source_bytes}),
            ));
        }
        let admission = self
            .state
            .admit_background_read(object.size_bytes as usize)
            .map_err(|error| format!("Audit admission unavailable: {error:?}"))?;
        let permit = self
            .state
            .image_jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Image processing capacity is busy".to_string())?;
        let key = crate::images::ImageKey::new(
            object.namespace.clone(),
            object.resource_id.clone(),
            object.slot,
        )
        .map_err(|error| format!("Invalid indexed image key: {error:?}"))?;
        let (metadata, bytes) = match self.state.store.read_snapshot(&key).await {
            Ok(snapshot) => snapshot,
            Err(ApiError::NotFound(_)) => {
                return Ok(AuditObjectResult::Changed(
                    json!({"reason":"object_missing"}),
                ));
            }
            Err(error) => return Err(format!("Could not read {}: {error:?}", object.key)),
        };
        if metadata.content_length != object.size_bytes
            || (object.etag.is_some() && metadata.etag != object.etag)
        {
            return Ok(AuditObjectResult::Changed(
                json!({"inventory_size_bytes":object.size_bytes,"current_size_bytes":metadata.content_length,"inventory_etag":object.etag,"current_etag":metadata.etag}),
            ));
        }
        let memory_permit = self
            .state
            .try_admit_image_processing(policy.output_width, policy.output_height)
            .map_err(|_| "Image processing memory budget is busy".to_string())?;
        let max_decode_dimension = self.state.max_decode_dimension;
        let max_decode_allocation_bytes = self.state.max_decode_allocation_bytes;
        let inspect = tokio::task::spawn_blocking(move || {
            let _memory_permit = memory_permit;
            processor_image::inspect_image_with_limits(
                &bytes,
                max_decode_dimension,
                max_decode_allocation_bytes,
            )
            .map(|inspection| (inspection, bytes.len()))
        })
        .await
        .map_err(|error| format!("Image inspection task failed: {error}"))?;
        drop(permit);
        drop(admission);
        let (inspection, size_bytes) = match inspect {
            Ok(result) => result,
            Err(error) => {
                return Ok(AuditObjectResult::Finding(
                    "invalid_or_unsupported_image".into(),
                    json!({"size_bytes":metadata.content_length,"error":error.to_string()}),
                ));
            }
        };
        let mut violations = Vec::new();
        if inspection.format != "webp" {
            violations.push("format");
        }
        if inspection.animated {
            violations.push("animated_image");
        }
        if inspection.width > policy.output_width || inspection.height > policy.output_height {
            violations.push("dimensions_exceed_policy");
        }
        if size_bytes > policy.max_output_bytes {
            violations.push("output_size_exceeds_policy");
        }
        let observed = json!({"format":inspection.format,"width":inspection.width,"height":inspection.height,"animated":inspection.animated,"size_bytes":size_bytes,"policy":{"max_output_bytes":policy.max_output_bytes,"max_width":policy.output_width,"max_height":policy.output_height,"profile":policy.profile_name}});
        if violations.is_empty() {
            Ok(AuditObjectResult::Compliant(observed))
        } else {
            Ok(AuditObjectResult::Finding(violations.join(","), observed))
        }
    }
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
        if let Some(state) = &self.state {
            for object in self.catalog.scan_objects(scan_id.clone()).await? {
                enqueue_verification(state, &self.catalog, object)
                    .await
                    .map_err(|error| format!("Could not enqueue object verification: {error:?}"))?;
            }
        }
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

    #[test]
    fn findings_cursor_is_url_safe_and_validated() {
        let encoded = encode_findings_cursor("products/845/01.webp", "output_size_exceeds_policy");
        assert!(encoded.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            decode_findings_cursor(&encoded).unwrap(),
            (
                "products/845/01.webp".to_string(),
                "output_size_exceeds_policy".to_string()
            )
        );
        assert!(decode_findings_cursor("bad-cursor").is_err());
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
    async fn mutation_journal_updates_usage_idempotently_for_slot_and_resource_changes() {
        let (_directory, catalog) = test_catalog();
        catalog.begin_scan("baseline").await.unwrap();
        catalog
            .upsert_batch(
                "baseline",
                vec![
                    inventory_object("products/845/01.webp", "products", "845", 1, 100),
                    inventory_object("products/845/02.webp", "products", "845", 2, 200),
                ],
            )
            .await
            .unwrap();
        catalog.finish_scan("baseline").await.unwrap();

        catalog
            .journal_mutation(
                "put-1".into(),
                "PUT",
                "products".into(),
                "845".into(),
                Some(1),
            )
            .await
            .unwrap();
        let mut changed = inventory_object("products/845/01.webp", "products", "845", 1, 150);
        changed.etag = Some("v2".into());
        catalog
            .apply_mutation(
                "put-1".into(),
                "products".into(),
                "845".into(),
                Some(1),
                Some(changed.clone()),
            )
            .await
            .unwrap();
        catalog
            .apply_mutation(
                "put-1".into(),
                "products".into(),
                "845".into(),
                Some(1),
                Some(changed),
            )
            .await
            .unwrap();
        let usage = catalog
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
        assert_eq!((usage.object_count, usage.total_bytes), (2, 350));

        catalog
            .journal_mutation(
                "delete-slot".into(),
                "DELETE",
                "products".into(),
                "845".into(),
                Some(2),
            )
            .await
            .unwrap();
        catalog
            .apply_mutation(
                "delete-slot".into(),
                "products".into(),
                "845".into(),
                Some(2),
                None,
            )
            .await
            .unwrap();
        catalog
            .journal_mutation(
                "delete-resource".into(),
                "DELETE",
                "products".into(),
                "845".into(),
                None,
            )
            .await
            .unwrap();
        catalog
            .apply_mutation(
                "delete-resource".into(),
                "products".into(),
                "845".into(),
                None,
                None,
            )
            .await
            .unwrap();
        let usage = catalog
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
        assert_eq!((usage.object_count, usage.total_bytes), (0, 0));
        assert!(catalog.pending_mutations().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn journal_entries_remain_pending_until_catalog_reconciliation() {
        let (_directory, catalog) = test_catalog();
        catalog
            .journal_mutation(
                "pending-op".into(),
                "POST",
                "products".into(),
                "845".into(),
                Some(1),
            )
            .await
            .unwrap();
        let pending = catalog.pending_mutations().await.unwrap();
        assert_eq!(
            pending,
            vec![(
                "pending-op".into(),
                "POST".into(),
                "products".into(),
                "845".into(),
                Some(1)
            )]
        );
    }

    #[tokio::test]
    async fn live_mutations_are_counted_before_the_first_full_inventory_finishes() {
        let (_directory, catalog) = test_catalog();
        catalog
            .journal_mutation(
                "first-post".into(),
                "POST",
                "products".into(),
                "845".into(),
                Some(1),
            )
            .await
            .unwrap();
        catalog
            .apply_mutation(
                "first-post".into(),
                "products".into(),
                "845".into(),
                Some(1),
                Some(inventory_object(
                    "products/845/01.webp",
                    "products",
                    "845",
                    1,
                    123,
                )),
            )
            .await
            .unwrap();
        let usage = catalog
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
        assert_eq!((usage.object_count, usage.total_bytes), (1, 123));
        assert!(usage.stale);
        assert_eq!(usage.scan_status, "never");
    }

    #[tokio::test]
    async fn verification_cache_requires_the_same_object_version_and_policy() {
        let (_directory, catalog) = test_catalog();
        let mut object = inventory_object("products/845/01.webp", "products", "845", 1, 123);
        object.etag = Some("etag-v1".into());
        catalog
            .record_verification(
                object.clone(),
                "policy-v1".into(),
                "compliant".into(),
                json!({"format":"webp"}),
            )
            .await
            .unwrap();
        assert!(
            catalog
                .verification_matches(&object, "policy-v1".into())
                .await
                .unwrap()
        );
        assert!(
            !catalog
                .verification_matches(&object, "policy-v2".into())
                .await
                .unwrap()
        );
        let mut changed = object.clone();
        changed.etag = Some("etag-v2".into());
        assert!(
            !catalog
                .verification_matches(&changed, "policy-v1".into())
                .await
                .unwrap()
        );
        changed.etag = object.etag.clone();
        changed.size_bytes += 1;
        assert!(
            !catalog
                .verification_matches(&changed, "policy-v1".into())
                .await
                .unwrap()
        );
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
    async fn audit_checkpoints_findings_and_retains_the_inventory_it_started_from() {
        let (_directory, catalog) = test_catalog();
        catalog.begin_scan("audit-source").await.unwrap();
        catalog
            .upsert_batch(
                "audit-source",
                vec![
                    inventory_object("products/845/01.webp", "products", "845", 1, 100),
                    inventory_object("products/845/02.webp", "products", "845", 2, 200),
                ],
            )
            .await
            .unwrap();
        catalog.finish_scan("audit-source").await.unwrap();

        let snapshot = AuditPolicySnapshot {
            default_max_input_bytes: 1024,
            default_max_output_bytes: 100,
            default_output_width: 1024,
            default_output_height: 1024,
            profiles: HashMap::new(),
            namespace_profiles: HashMap::new(),
        };
        catalog
            .create_audit(
                "audit-1".into(),
                AuditScope {
                    namespace: Some("products".into()),
                    resource_id: None,
                    slot: None,
                },
                snapshot,
                "fingerprint".into(),
            )
            .await
            .unwrap();
        let (scan_id, scope, _, cursor) = catalog.audit_payload("audit-1".into()).await.unwrap();
        assert_eq!(scan_id, "audit-source");
        let batch = catalog
            .audit_objects(scan_id.clone(), scope.clone(), cursor)
            .await
            .unwrap();
        assert_eq!(batch.len(), 2);
        catalog
            .record_audit_result(
                "audit-1".into(),
                batch[0].clone(),
                AuditObjectResult::Finding(
                    "output_size_exceeds_policy".into(),
                    json!({"size_bytes":100}),
                ),
            )
            .await
            .unwrap();
        let (_, _, _, cursor) = catalog.audit_payload("audit-1".into()).await.unwrap();
        assert_eq!(cursor.as_deref(), Some("products/845/01.webp"));
        let next = catalog.audit_objects(scan_id, scope, cursor).await.unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].key, "products/845/02.webp");
        catalog
            .record_audit_result(
                "audit-1".into(),
                batch[1].clone(),
                AuditObjectResult::Finding(
                    "dimensions_exceed_policy".into(),
                    json!({"width":2048}),
                ),
            )
            .await
            .unwrap();

        catalog.begin_scan("new-inventory").await.unwrap();
        catalog.finish_scan("new-inventory").await.unwrap();
        let retained = catalog
            .audit_objects("audit-source".into(), AuditScope::default(), None)
            .await
            .unwrap();
        assert_eq!(retained.len(), 2);

        let page = catalog
            .audit_findings("audit-1".into(), None, 1)
            .await
            .unwrap();
        assert_eq!(page.findings.len(), 1);
        assert_eq!(page.findings[0].code, "output_size_exceeds_policy");
        let cursor = page
            .next_cursor
            .expect("second finding should have a cursor");
        let (key, code) = decode_findings_cursor(&cursor).unwrap();
        let next_page = catalog
            .audit_findings("audit-1".into(), Some((key, code)), 1)
            .await
            .unwrap();
        assert_eq!(next_page.findings.len(), 1);
        assert_eq!(next_page.findings[0].code, "dimensions_exceed_policy");
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
        register_inventory_handler(
            &scheduler,
            catalog.clone(),
            operator,
            Arc::new(ActivityTracker::default()),
            0,
            None,
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
