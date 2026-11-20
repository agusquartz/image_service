//! Durable, single-Core job storage and in-process dispatch.
//!
//! SQLite is the source of truth. The in-memory dispatcher only claims a
//! bounded number of jobs and relies on leases to recover work after a crash.

use futures_util::FutureExt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    panic::AssertUnwindSafe,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{RwLock, watch},
    task::JoinSet,
    time,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;
const MAX_ERROR_LENGTH: usize = 2048;
const MAX_JOB_NAME_LENGTH: usize = 100;
const MAX_BACKOFF_SECONDS: u64 = 3600;
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Debug)]
pub struct JobError(String);

impl JobError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for JobError {}

impl From<rusqlite::Error> for JobError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for JobError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, JobError> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "retry_wait" => Ok(Self::RetryWait),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(JobError::new(format!(
                "Unknown persisted job state: {value}"
            ))),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NewJob {
    pub queue: String,
    pub kind: String,
    pub payload: Value,
    pub policy_snapshot: Value,
    pub dedupe_key: Option<String>,
    pub dependencies: Vec<String>,
    /// Higher values are selected first. Equal-priority jobs age in the queue.
    pub priority: i32,
    pub max_attempts: u32,
    pub run_after: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetJobMode {
    CreateOnly,
    Supersede,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetEnqueueResult {
    Enqueued {
        id: String,
        superseded_stage_id: Option<String>,
        superseded_was_running: bool,
    },
    ActiveConflict(String),
    ExistingIdempotent(String),
    IdempotencyConflict,
    CapacityExceeded,
}

#[derive(Clone, Debug)]
pub struct IntervalSchedule {
    pub id: String,
    pub queue: String,
    pub kind: String,
    pub payload: Value,
    pub policy_snapshot: Value,
    pub priority: i32,
    pub max_attempts: u32,
    pub interval: Duration,
    pub run_immediately: bool,
}

impl NewJob {
    pub fn system(queue: impl Into<String>, kind: impl Into<String>, payload: Value) -> Self {
        Self {
            queue: queue.into(),
            kind: kind.into(),
            payload,
            policy_snapshot: Value::Null,
            dedupe_key: None,
            dependencies: Vec::new(),
            priority: 0,
            max_attempts: 5,
            run_after: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct JobRecord {
    pub id: String,
    pub queue: String,
    pub kind: String,
    pub parent_id: Option<String>,
    pub dedupe_key: Option<String>,
    pub payload: Value,
    pub policy_snapshot: Value,
    pub status: JobStatus,
    pub priority: i32,
    pub attempts: u32,
    pub max_attempts: u32,
    pub lease_owner: Option<String>,
    pub lease_until: Option<SystemTime>,
    pub run_after: SystemTime,
    pub progress: Value,
    pub result: Option<Value>,
    pub last_error: Option<String>,
    pub cancel_requested: bool,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

#[derive(Clone)]
pub struct JobStore {
    connection: Arc<Mutex<Connection>>,
}

impl fmt::Debug for JobStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("JobStore").finish_non_exhaustive()
    }
}

impl JobStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JobError> {
        let path = path.as_ref();
        if path != Path::new(":memory:")
            && let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                JobError::new(format!("Could not create jobs database directory: {error}"))
            })?;
        }

        if path != Path::new(":memory:") {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path).map_err(|error| {
                JobError::new(format!("Could not create jobs database file: {error}"))
            })?;
        }

        let mut connection = Connection::open(path)?;
        #[cfg(unix)]
        if path != Path::new(":memory:") {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, permissions).map_err(|error| {
                JobError::new(format!(
                    "Could not restrict jobs database permissions: {error}"
                ))
            })?;
        }
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        migrate(&mut connection)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub async fn enqueue(&self, job: NewJob) -> Result<String, JobError> {
        validate_job_name("queue", &job.queue)?;
        validate_job_name("kind", &job.kind)?;
        if job.max_attempts == 0 || job.max_attempts > 100 {
            return Err(JobError::new("max_attempts must be between 1 and 100"));
        }
        if !(-100..=100).contains(&job.priority) {
            return Err(JobError::new("job priority must be between -100 and 100"));
        }
        if job.dependencies.len() > 256 {
            return Err(JobError::new(
                "a job cannot have more than 256 dependencies",
            ));
        }
        let payload = serde_json::to_string(&job.payload)?;
        let policy = serde_json::to_string(&job.policy_snapshot)?;
        let progress = "{}".to_string();
        let now = now_ms();
        let run_after = system_time_ms(job.run_after.unwrap_or_else(SystemTime::now));
        let connection = self.connection.clone();

        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(dedupe_key) = job.dedupe_key.as_deref()
                && let Some(existing) = transaction
                    .query_row(
                        "SELECT id FROM jobs WHERE dedupe_key = ?1 AND state IN ('pending', 'running', 'retry_wait')",
                        [dedupe_key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
            {
                return Ok(existing);
            }

            for dependency in &job.dependencies {
                let exists = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = ?1)",
                    [dependency],
                    |row| row.get::<_, bool>(0),
                )?;
                if !exists {
                    return Err(JobError::new(format!(
                        "job dependency does not exist: {dependency}"
                    )));
                }
            }

            let id = Uuid::new_v4().to_string();
            let parent_id = job.dependencies.first().cloned();
            transaction.execute(
                "INSERT INTO jobs (id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, run_after_ms, progress_json, created_at_ms, updated_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, 0, ?9, ?10, ?11, ?12, ?12)",
                params![
                    id,
                    job.queue,
                    job.kind,
                    parent_id,
                    job.dedupe_key,
                    payload,
                    policy,
                    job.priority,
                    job.max_attempts,
                    run_after,
                    progress,
                    now
                ],
            )?;
            for dependency in &job.dependencies {
                transaction.execute(
                    "INSERT INTO job_dependencies(job_id, depends_on_id) VALUES (?1, ?2)",
                    params![id, dependency],
                )?;
            }
            transaction.execute(
                "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, 'enqueued', ?2, '{}')",
                params![id, now],
            )?;
            transaction.commit()?;
            Ok(id)
        })
        .await
    }

    /// Enqueues a job that owns one logical target. The active dedupe key is
    /// the target reservation: create-only jobs conflict, while replacement
    /// jobs cancel or fence the previous owner in the same SQLite transaction.
    pub async fn enqueue_targeted(
        &self,
        job: NewJob,
        mode: TargetJobMode,
        max_active_jobs: usize,
        max_active_bytes: u64,
        job_bytes: u64,
    ) -> Result<TargetEnqueueResult, JobError> {
        validate_job_name("queue", &job.queue)?;
        validate_job_name("kind", &job.kind)?;
        let dedupe_key = job
            .dedupe_key
            .clone()
            .ok_or_else(|| JobError::new("targeted jobs require a dedupe key"))?;
        if job.max_attempts == 0 || job.max_attempts > 100 {
            return Err(JobError::new("max_attempts must be between 1 and 100"));
        }
        let payload = serde_json::to_string(&job.payload)?;
        let idempotency_key = job
            .payload
            .get("idempotency_key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let request_fingerprint = job
            .payload
            .get("request_fingerprint")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let policy = serde_json::to_string(&job.policy_snapshot)?;
        let progress = "{}".to_string();
        let now = now_ms();
        let run_after = system_time_ms(job.run_after.unwrap_or_else(SystemTime::now));
        let connection = self.connection.clone();

        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(idempotency_key) = &idempotency_key {
                let existing: Option<(String, String)> = transaction
                    .query_row(
                        "SELECT id,payload_json FROM jobs WHERE json_extract(payload_json,'$.idempotency_key')=?1 ORDER BY created_at_ms DESC LIMIT 1",
                        [idempotency_key],
                        |row| Ok((row.get(0)?,row.get(1)?)),
                    )
                    .optional()?;
                if let Some((id, existing_payload)) = existing {
                    let existing_fingerprint = serde_json::from_str::<serde_json::Value>(&existing_payload)
                        .ok()
                        .and_then(|payload| payload.get("request_fingerprint")?.as_str().map(str::to_owned));
                    if existing_fingerprint == request_fingerprint {
                        return Ok(TargetEnqueueResult::ExistingIdempotent(id));
                    }
                    return Ok(TargetEnqueueResult::IdempotencyConflict);
                }
            }
            let active: Option<(String, String, String)> = transaction
                .query_row(
                    "SELECT id, state, payload_json FROM jobs WHERE dedupe_key = ?1 AND state IN ('pending','running','retry_wait')",
                    [&dedupe_key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let mut superseded_stage_id = None;
            let mut superseded_was_running = false;
            if let Some((active_id, active_state, active_payload)) = &active {
                if mode == TargetJobMode::CreateOnly {
                    return Ok(TargetEnqueueResult::ActiveConflict(active_id.clone()));
                }
                superseded_stage_id = serde_json::from_str::<serde_json::Value>(active_payload)
                    .ok()
                    .and_then(|payload| payload.get("stage_id")?.as_str().map(str::to_owned));
                if active_state == "running" {
                    superseded_was_running = true;
                    let fenced_key = format!("{dedupe_key}:superseded:{active_id}");
                    transaction.execute(
                        "UPDATE jobs SET dedupe_key=?2,cancel_requested=1,last_error='superseded by a newer target job',updated_at_ms=?3 WHERE id=?1",
                        params![active_id, fenced_key, now],
                    )?;
                    transaction.execute(
                        "INSERT INTO job_events(job_id,event,occurred_at_ms,detail_json) VALUES (?1,'superseded',?2,'{}')",
                        params![active_id, now],
                    )?;
                } else {
                    transaction.execute(
                        "UPDATE jobs SET state='cancelled',last_error='superseded by a newer target job',updated_at_ms=?2,finished_at_ms=?2 WHERE id=?1",
                        params![active_id, now],
                    )?;
                    transaction.execute(
                        "INSERT INTO job_events(job_id,event,occurred_at_ms,detail_json) VALUES (?1,'superseded',?2,'{}')",
                        params![active_id, now],
                    )?;
                }
            }

            let (active_count, active_bytes): (i64, i64) = transaction.query_row(
                "SELECT COUNT(*),COALESCE(SUM(CAST(json_extract(payload_json,'$.source_bytes') AS INTEGER)),0) FROM jobs WHERE queue=?1 AND state IN ('pending','running','retry_wait')",
                [&job.queue],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            // A superseded pending job was marked cancelled above and is no
            // longer counted. A superseded running job remains active until
            // its handler observes cancellation and reaches a terminal state;
            // keep its source bytes in the capacity total during that window.
            let active_count = active_count.max(0) as usize;
            let active_bytes = active_bytes.max(0) as u64;
            if active_count >= max_active_jobs
                || active_bytes.saturating_add(job_bytes) > max_active_bytes
            {
                return Ok(TargetEnqueueResult::CapacityExceeded);
            }

            let id = Uuid::new_v4().to_string();
            let parent_id = job.dependencies.first().cloned();
            transaction.execute(
                "INSERT INTO jobs (id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, run_after_ms, progress_json, created_at_ms, updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,0,?9,?10,?11,?12,?12)",
                params![id,job.queue,job.kind,parent_id,dedupe_key,payload,policy,job.priority,job.max_attempts,run_after,progress,now],
            )?;
            transaction.execute(
                "INSERT INTO job_events(job_id,event,occurred_at_ms,detail_json) VALUES (?1,'enqueued',?2,'{}')",
                params![id,now],
            )?;
            transaction.commit()?;
            Ok(TargetEnqueueResult::Enqueued {
                id,
                superseded_stage_id,
                superseded_was_running,
            })
        })
        .await
    }

    pub async fn cancel_target(
        &self,
        dedupe_key: String,
    ) -> Result<Option<(String, bool)>, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active: Option<(String, String, String)> = transaction
                .query_row(
                    "SELECT id,state,payload_json FROM jobs WHERE dedupe_key=?1 AND state IN ('pending','running','retry_wait')",
                    [&dedupe_key],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
                )
                .optional()?;
            let mut cancelled_stage = None;
            if let Some((id,state,payload)) = active {
                let now = now_ms();
                let stage_id = serde_json::from_str::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|payload| payload.get("stage_id")?.as_str().map(str::to_owned));
                if state == "running" {
                    transaction.execute("UPDATE jobs SET dedupe_key=?2,cancel_requested=1,updated_at_ms=?3 WHERE id=?1", params![id,format!("{dedupe_key}:cancelled:{id}"),now])?;
                } else {
                    transaction.execute("UPDATE jobs SET state='cancelled',last_error='cancelled by a synchronous image mutation',updated_at_ms=?2,finished_at_ms=?2 WHERE id=?1",params![id,now])?;
                }
                transaction.execute("INSERT INTO job_events(job_id,event,occurred_at_ms,detail_json) VALUES (?1,'cancel_requested',?2,'{}')",params![id,now])?;
                cancelled_stage = stage_id.map(|stage| (stage, state == "running"));
            }
            transaction.commit()?;
            Ok(cancelled_stage)
        }).await
    }

    pub async fn cancellation_requested(&self, id: String) -> Result<bool, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            connection
                .query_row(
                    "SELECT cancel_requested FROM jobs WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        })
        .await
    }

    pub async fn register_interval_schedule(
        &self,
        schedule: IntervalSchedule,
    ) -> Result<(), JobError> {
        validate_job_name("schedule id", &schedule.id)?;
        validate_job_name("queue", &schedule.queue)?;
        validate_job_name("kind", &schedule.kind)?;
        if schedule.interval < Duration::from_secs(1)
            || schedule.interval > Duration::from_secs(31 * 24 * 60 * 60)
        {
            return Err(JobError::new(
                "schedule interval must be between 1 second and 31 days",
            ));
        }
        if !(1..=100).contains(&schedule.max_attempts) {
            return Err(JobError::new("max_attempts must be between 1 and 100"));
        }
        if !(-100..=100).contains(&schedule.priority) {
            return Err(JobError::new("job priority must be between -100 and 100"));
        }
        let payload = serde_json::to_string(&schedule.payload)?;
        let policy = serde_json::to_string(&schedule.policy_snapshot)?;
        let interval_ms = schedule.interval.as_millis().min(i64::MAX as u128) as i64;
        let next_run = if schedule.run_immediately {
            now_ms()
        } else {
            now_ms().saturating_add(interval_ms)
        };
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            connection.execute(
                "INSERT INTO job_schedules (id, queue, kind, payload_json, policy_json, priority, max_attempts, interval_ms, next_run_ms, enabled, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10) ON CONFLICT(id) DO UPDATE SET queue = excluded.queue, kind = excluded.kind, payload_json = excluded.payload_json, policy_json = excluded.policy_json, priority = excluded.priority, max_attempts = excluded.max_attempts, interval_ms = excluded.interval_ms, enabled = 1, updated_at_ms = excluded.updated_at_ms",
                params![
                    schedule.id,
                    schedule.queue,
                    schedule.kind,
                    payload,
                    policy,
                    schedule.priority,
                    schedule.max_attempts,
                    interval_ms,
                    next_run,
                    now_ms()
                ],
            )?;
            Ok(())
        })
        .await
    }

    async fn materialize_due_schedules(&self) -> Result<usize, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = now_ms();
            let mut statement = transaction.prepare(
                "SELECT id, queue, kind, payload_json, policy_json, priority, max_attempts, interval_ms FROM job_schedules WHERE enabled = 1 AND next_run_ms <= ?1 ORDER BY next_run_ms LIMIT 100",
            )?;
            let due = statement
                .query_map([now], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i32>(5)?,
                        row.get::<_, u32>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);

            let mut created = 0;
            for (id, queue, kind, payload, policy, priority, max_attempts, interval_ms) in due {
                let payload: Value = serde_json::from_str(&payload)?;
                let policy: Value = serde_json::from_str(&policy)?;
                let payload_json = serde_json::to_string(&payload)?;
                let policy_json = serde_json::to_string(&policy)?;
                let dedupe_key = format!("schedule:{id}");
                let already_active = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE dedupe_key = ?1 AND state IN ('pending','running','retry_wait'))",
                    [&dedupe_key],
                    |row| row.get::<_, bool>(0),
                )?;
                if !already_active {
                    let job_id = Uuid::new_v4().to_string();
                    transaction.execute(
                        "INSERT INTO jobs (id, queue, kind, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, run_after_ms, progress_json, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, 0, ?8, ?9, '{}', ?9, ?9)",
                        params![job_id, queue, kind, dedupe_key, payload_json, policy_json, priority, max_attempts, now],
                    )?;
                    transaction.execute(
                        "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, 'enqueued', ?2, json_object('schedule_id', ?3))",
                        params![job_id, now, id],
                    )?;
                    created += 1;
                }
                transaction.execute(
                    "UPDATE job_schedules SET next_run_ms = ?2, updated_at_ms = ?2 WHERE id = ?1",
                    params![id, now.saturating_add(interval_ms)],
                )?;
            }
            transaction.commit()?;
            Ok(created)
        })
        .await
    }

    pub async fn get(&self, id: impl Into<String>) -> Result<Option<JobRecord>, JobError> {
        let id = id.into();
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            connection
                .query_row(
                    "SELECT id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, lease_owner, lease_until_ms, run_after_ms, progress_json, result_json, last_error, cancel_requested, created_at_ms, updated_at_ms FROM jobs WHERE id = ?1",
                    [id],
                    row_to_job,
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn get_active_by_dedupe_key(
        &self,
        dedupe_key: String,
    ) -> Result<Option<JobRecord>, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            connection
                .query_row(
                    "SELECT id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, lease_owner, lease_until_ms, run_after_ms, progress_json, result_json, last_error, cancel_requested, created_at_ms, updated_at_ms FROM jobs WHERE dedupe_key=?1 AND state IN ('pending','running','retry_wait')",
                    [dedupe_key],
                    row_to_job,
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn get_by_idempotency_key(
        &self,
        idempotency_key: String,
    ) -> Result<Option<JobRecord>, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            connection
                .query_row(
                    "SELECT id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, lease_owner, lease_until_ms, run_after_ms, progress_json, result_json, last_error, cancel_requested, created_at_ms, updated_at_ms FROM jobs WHERE json_extract(payload_json,'$.idempotency_key')=?1 ORDER BY created_at_ms DESC LIMIT 1",
                    [idempotency_key],
                    row_to_job,
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn list(
        &self,
        queue: Option<String>,
        status: Option<JobStatus>,
        limit: u32,
    ) -> Result<Vec<JobRecord>, JobError> {
        if !(1..=1000).contains(&limit) {
            return Err(JobError::new("job list limit must be between 1 and 1000"));
        }
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, lease_owner, lease_until_ms, run_after_ms, progress_json, result_json, last_error, cancel_requested, created_at_ms, updated_at_ms FROM jobs WHERE (?1 IS NULL OR queue = ?1) AND (?2 IS NULL OR state = ?2) ORDER BY created_at_ms DESC LIMIT ?3",
            )?;
            let mut rows = statement.query(params![
                queue,
                status.as_ref().map(JobStatus::as_str),
                limit
            ])?;
            let mut jobs = Vec::new();
            while let Some(row) = rows.next()? {
                jobs.push(row_to_job(row)?);
            }
            Ok(jobs)
        })
        .await
    }

    pub async fn request_cancel(&self, id: impl Into<String>) -> Result<bool, JobError> {
        let id = id.into();
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = now_ms();
            let changed = transaction.execute(
                "UPDATE jobs SET state = 'cancelled', updated_at_ms = ?2, finished_at_ms = ?2, lease_owner = NULL, lease_until_ms = NULL WHERE id = ?1 AND state IN ('pending', 'retry_wait')",
                params![id, now],
            )?;
            if changed > 0 {
                transaction.execute(
                    "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, 'cancelled', ?2, '{}')",
                    params![id, now],
                )?;
                transaction.commit()?;
                return Ok(true);
            }
            let changed = transaction.execute(
                "UPDATE jobs SET cancel_requested = 1, updated_at_ms = ?2 WHERE id = ?1 AND state = 'running'",
                params![id, now],
            )?;
            if changed > 0 {
                transaction.execute(
                    "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, 'cancel_requested', ?2, '{}')",
                    params![id, now],
                )?;
            }
            transaction.commit()?;
            Ok(changed > 0)
        })
        .await
    }

    async fn prune_terminal(&self, retention_days: u64) -> Result<usize, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let retention_ms = retention_days
                .saturating_mul(86_400_000)
                .min(i64::MAX as u64) as i64;
            let cutoff = now_ms().saturating_sub(retention_ms);
            connection
                .execute(
                    "DELETE FROM jobs WHERE state IN ('succeeded','failed','cancelled') AND finished_at_ms <= ?1 AND NOT EXISTS (SELECT 1 FROM job_dependencies d WHERE d.depends_on_id = jobs.id)",
                    [cutoff],
                )
                .map_err(Into::into)
        })
        .await
    }

    async fn claim(
        &self,
        worker: String,
        kinds: Vec<String>,
        lease_duration: Duration,
    ) -> Result<Option<JobRecord>, JobError> {
        if kinds.is_empty() {
            return Ok(None);
        }
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = now_ms();
            transaction.execute(
                "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) SELECT id, 'lease_expired', ?1, json_object('retryable', attempts < max_attempts) FROM jobs WHERE state = 'running' AND lease_until_ms <= ?1",
                [now],
            )?;
            transaction.execute(
                "UPDATE jobs SET state = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'retry_wait' END, run_after_ms = ?1, lease_owner = NULL, lease_until_ms = NULL, updated_at_ms = ?1, finished_at_ms = CASE WHEN attempts >= max_attempts THEN ?1 ELSE NULL END, last_error = 'worker lease expired' WHERE state = 'running' AND lease_until_ms <= ?1",
                [now],
            )?;
            transaction.execute(
                "UPDATE jobs SET state = 'cancelled', updated_at_ms = ?1, finished_at_ms = ?1, last_error = 'dependency did not succeed' WHERE state IN ('pending', 'retry_wait') AND EXISTS (SELECT 1 FROM job_dependencies d JOIN jobs parent ON parent.id = d.depends_on_id WHERE d.job_id = jobs.id AND parent.state IN ('failed', 'cancelled'))",
                [now],
            )?;

            let placeholders = vec!["?"; kinds.len()].join(", ");
            let query = format!(
                "SELECT j.id FROM jobs j WHERE j.kind IN ({placeholders}) AND j.state IN ('pending', 'retry_wait') AND j.run_after_ms <= ? AND NOT EXISTS (SELECT 1 FROM job_dependencies d JOIN jobs parent ON parent.id = d.depends_on_id WHERE d.job_id = j.id AND parent.state <> 'succeeded') ORDER BY (j.priority + MIN((? - j.created_at_ms) / 60000, 200)) DESC, j.created_at_ms ASC LIMIT 1"
            );
            let mut query_params: Vec<&dyn rusqlite::ToSql> =
                kinds.iter().map(|kind| kind as &dyn rusqlite::ToSql).collect();
            query_params.push(&now);
            query_params.push(&now);
            let id = transaction
                .query_row(&query, query_params.as_slice(), |row| row.get::<_, String>(0))
                .optional()?;
            let Some(id) = id else {
                transaction.commit()?;
                return Ok(None);
            };

            let lease_until = now.saturating_add(lease_duration.as_millis() as i64);
            transaction.execute(
                "UPDATE jobs SET state = 'running', attempts = attempts + 1, lease_owner = ?2, lease_until_ms = ?3, updated_at_ms = ?4, started_at_ms = COALESCE(started_at_ms, ?4), cancel_requested = 0 WHERE id = ?1",
                params![id, worker, lease_until, now],
            )?;
            transaction.execute(
                "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, 'claimed', ?2, json_object('worker', ?3))",
                params![id, now, worker],
            )?;
            let job = transaction.query_row(
                "SELECT id, queue, kind, parent_id, dedupe_key, payload_json, policy_json, state, priority, attempts, max_attempts, lease_owner, lease_until_ms, run_after_ms, progress_json, result_json, last_error, cancel_requested, created_at_ms, updated_at_ms FROM jobs WHERE id = ?1",
                [&id],
                row_to_job,
            )?;
            transaction.commit()?;
            Ok(Some(job))
        })
        .await
    }

    async fn heartbeat(
        &self,
        id: String,
        owner: String,
        lease_duration: Duration,
    ) -> Result<Option<bool>, JobError> {
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let now = now_ms();
            let until = now.saturating_add(lease_duration.as_millis() as i64);
            let changed = connection.execute(
                "UPDATE jobs SET lease_until_ms = ?3, updated_at_ms = ?4 WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
                params![id, owner, until, now],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            connection
                .query_row(
                    "SELECT cancel_requested FROM jobs WHERE id = ?1 AND lease_owner = ?2",
                    params![id, owner],
                    |row| row.get::<_, bool>(0),
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    async fn checkpoint(&self, id: String, owner: String, progress: Value) -> Result<(), JobError> {
        let json = serde_json::to_string(&progress)?;
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let changed = connection.execute(
                "UPDATE jobs SET progress_json = ?3, updated_at_ms = ?4 WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
                params![id, owner, json, now_ms()],
            )?;
            if changed == 0 {
                return Err(JobError::new("job lease was lost before checkpoint"));
            }
            Ok(())
        })
        .await
    }

    async fn finish(&self, id: String, owner: String, result: Value) -> Result<bool, JobError> {
        let result = serde_json::to_string(&result)?;
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let now = now_ms();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = transaction.execute(
                "UPDATE jobs SET state = CASE WHEN cancel_requested = 1 THEN 'cancelled' ELSE 'succeeded' END, result_json = ?3, lease_owner = NULL, lease_until_ms = NULL, updated_at_ms = ?4, finished_at_ms = ?4 WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
                params![id, owner, result, now],
            )?;
            if changed > 0 {
                let state = transaction.query_row(
                    "SELECT state FROM jobs WHERE id = ?1",
                    [&id],
                    |row| row.get::<_, String>(0),
                )?;
                transaction.execute(
                    "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, ?2, ?3, ?4)",
                    params![id, state, now, result],
                )?;
            }
            transaction.commit()?;
            Ok(changed > 0)
        })
        .await
    }

    async fn fail(
        &self,
        id: String,
        owner: String,
        message: String,
        retryable: bool,
    ) -> Result<Option<JobStatus>, JobError> {
        let message: String = message.chars().take(MAX_ERROR_LENGTH).collect();
        let connection = self.connection.clone();
        run_blocking(connection, move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = transaction
                .query_row(
                    "SELECT attempts, max_attempts, cancel_requested FROM jobs WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
                    params![id, owner],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?, row.get::<_, bool>(2)?)),
                )
                .optional()?;
            let Some((attempts, max_attempts, cancel_requested)) = current else {
                transaction.commit()?;
                return Ok(None);
            };
            let now = now_ms();
            let status = if cancel_requested {
                JobStatus::Cancelled
            } else if retryable && attempts < max_attempts {
                JobStatus::RetryWait
            } else {
                JobStatus::Failed
            };
            let delay = if status == JobStatus::RetryWait {
                let seconds = 2_u64
                    .saturating_pow(attempts.saturating_sub(1).min(10))
                    .min(MAX_BACKOFF_SECONDS);
                seconds.saturating_mul(1000) as i64
            } else {
                0
            };
            transaction.execute(
                "UPDATE jobs SET state = ?3, last_error = ?4, run_after_ms = ?5, lease_owner = NULL, lease_until_ms = NULL, updated_at_ms = ?6, finished_at_ms = CASE WHEN ?3 IN ('failed', 'cancelled') THEN ?6 ELSE NULL END WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
                params![id, owner, status.as_str(), message, now.saturating_add(delay), now],
            )?;
            transaction.execute(
                "INSERT INTO job_events(job_id, event, occurred_at_ms, detail_json) VALUES (?1, ?2, ?3, json_object('message', ?4))",
                params![id, status.as_str(), now, message],
            )?;
            transaction.commit()?;
            Ok(Some(status))
        })
        .await
    }
}

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub concurrency: usize,
    pub idle_seconds: u64,
    pub poll_interval: Duration,
    pub lease_duration: Duration,
    pub daily_utc_window: Option<DailyUtcWindow>,
    pub retention_days: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            concurrency: 1,
            idle_seconds: 30,
            poll_interval: Duration::from_secs(1),
            lease_duration: Duration::from_secs(60),
            daily_utc_window: None,
            retention_days: 30,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DailyUtcWindow {
    start_minute: u16,
    end_minute: u16,
}

impl DailyUtcWindow {
    pub fn parse(value: &str) -> Result<Self, JobError> {
        let (start, end) = value
            .split_once('-')
            .ok_or_else(|| JobError::new("UTC background window must use HH:MM-HH:MM"))?;
        let start_minute = parse_minute(start)?;
        let end_minute = parse_minute(end)?;
        if start_minute == end_minute {
            return Err(JobError::new(
                "UTC background window cannot have equal endpoints",
            ));
        }
        Ok(Self {
            start_minute,
            end_minute,
        })
    }

    pub fn is_open_at(self, now: SystemTime) -> bool {
        let seconds = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() % 86_400;
        let minute = (seconds / 60) as u16;
        if self.start_minute < self.end_minute {
            (self.start_minute..self.end_minute).contains(&minute)
        } else {
            minute >= self.start_minute || minute < self.end_minute
        }
    }
}

fn parse_minute(value: &str) -> Result<u16, JobError> {
    let (hours, minutes) = value
        .split_once(':')
        .ok_or_else(|| JobError::new("UTC background window must use HH:MM-HH:MM"))?;
    if hours.len() != 2 || minutes.len() != 2 {
        return Err(JobError::new("UTC background window must use HH:MM-HH:MM"));
    }
    let hours = hours
        .parse::<u16>()
        .map_err(|_| JobError::new("invalid hour in UTC background window"))?;
    let minutes = minutes
        .parse::<u16>()
        .map_err(|_| JobError::new("invalid minute in UTC background window"))?;
    if hours > 23 || minutes > 59 {
        return Err(JobError::new("UTC background window time is out of range"));
    }
    Ok(hours * 60 + minutes)
}

#[derive(Debug)]
pub struct ActivityTracker {
    active_requests: AtomicUsize,
    last_activity_ms: AtomicU64,
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self {
            active_requests: AtomicUsize::new(0),
            last_activity_ms: AtomicU64::new(now_ms().max(0) as u64),
        }
    }
}

impl ActivityTracker {
    pub fn enter(self: &Arc<Self>) -> ActivityGuard {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        self.touch();
        ActivityGuard {
            tracker: self.clone(),
        }
    }

    pub fn is_idle_for(&self, duration: Duration) -> bool {
        if self.active_requests.load(Ordering::Acquire) != 0 {
            return false;
        }
        let last = self.last_activity_ms.load(Ordering::Acquire);
        (now_ms().max(0) as u64).saturating_sub(last) >= duration.as_millis() as u64
    }

    fn touch(&self) {
        self.last_activity_ms
            .store(now_ms().max(0) as u64, Ordering::Release);
    }
}

pub struct ActivityGuard {
    tracker: Arc<ActivityTracker>,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        self.tracker.touch();
        self.tracker.active_requests.fetch_sub(1, Ordering::AcqRel);
    }
}

pub enum JobOutcome {
    Succeeded(Value),
    Failed { message: String, retryable: bool },
}

pub type JobFuture<'a> = Pin<Box<dyn Future<Output = JobOutcome> + Send + 'a>>;

pub trait JobHandler: Send + Sync + 'static {
    fn execute<'a>(&'a self, context: JobContext) -> JobFuture<'a>;

    fn after_terminal<'a>(
        &'a self,
        _job: &'a JobRecord,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    /// Whether jobs of this kind may run while foreground HTTP requests are
    /// active. The default keeps maintenance work behind the idle gate.
    fn run_during_foreground_activity(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct JobContext {
    pub job: JobRecord,
    owner: String,
    store: JobStore,
    cancellation: watch::Receiver<bool>,
}

impl JobContext {
    pub async fn checkpoint(&self, progress: Value) -> Result<(), JobError> {
        self.store
            .checkpoint(self.job.id.clone(), self.owner.clone(), progress)
            .await
    }

    pub fn cancellation_requested(&self) -> bool {
        *self.cancellation.borrow()
    }

    pub async fn cancelled(&mut self) -> Result<(), watch::error::RecvError> {
        while !*self.cancellation.borrow() {
            self.cancellation.changed().await?;
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct JobScheduler {
    store: JobStore,
    config: SchedulerConfig,
    handlers: Arc<RwLock<HashMap<String, Arc<dyn JobHandler>>>>,
    last_prune_ms: Arc<AtomicU64>,
}

impl fmt::Debug for JobScheduler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobScheduler")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl JobScheduler {
    pub fn open(path: impl AsRef<Path>, config: SchedulerConfig) -> Result<Self, JobError> {
        if config.concurrency == 0 || config.concurrency > 64 {
            return Err(JobError::new("job concurrency must be between 1 and 64"));
        }
        if config.lease_duration < Duration::from_secs(3) {
            return Err(JobError::new(
                "job lease duration must be at least 3 seconds",
            ));
        }
        if !(1..=3650).contains(&config.retention_days) {
            return Err(JobError::new(
                "job retention must be between 1 and 3650 days",
            ));
        }
        Ok(Self {
            store: JobStore::open(path)?,
            config,
            handlers: Arc::new(RwLock::new(HashMap::new())),
            last_prune_ms: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn store(&self) -> JobStore {
        self.store.clone()
    }

    pub async fn register_handler(
        &self,
        kind: impl Into<String>,
        handler: Arc<dyn JobHandler>,
    ) -> Result<(), JobError> {
        let kind = kind.into();
        validate_job_name("kind", &kind)?;
        self.handlers.write().await.insert(kind, handler);
        Ok(())
    }

    pub fn start(
        &self,
        activity: Arc<ActivityTracker>,
        service_ready: Arc<std::sync::atomic::AtomicBool>,
    ) -> JobRuntime {
        let (stop, receiver) = watch::channel(false);
        let mut tasks = JoinSet::new();
        for index in 0..self.config.concurrency {
            let scheduler = self.clone();
            let activity = activity.clone();
            let service_ready = service_ready.clone();
            let receiver = receiver.clone();
            tasks.spawn(async move {
                scheduler
                    .worker_loop(index, activity, service_ready, receiver)
                    .await;
            });
        }
        JobRuntime { stop, tasks }
    }

    async fn worker_loop(
        &self,
        index: usize,
        activity: Arc<ActivityTracker>,
        service_ready: Arc<std::sync::atomic::AtomicBool>,
        mut stop: watch::Receiver<bool>,
    ) {
        let worker_id = format!("{}-{index}", Uuid::new_v4());
        loop {
            if *stop.borrow() {
                break;
            }
            if let Err(error) = self.store.materialize_due_schedules().await {
                tracing::error!(error = %error, "could not materialize due scheduled jobs");
            }
            if !service_ready.load(Ordering::Acquire)
                || self
                    .config
                    .daily_utc_window
                    .is_some_and(|window| !window.is_open_at(SystemTime::now()))
            {
                tokio::select! {
                    _ = stop.changed() => {},
                    _ = time::sleep(self.config.poll_interval) => {},
                }
                continue;
            }
            let handlers = self.handlers.read().await.clone();
            if handlers.is_empty() {
                tokio::select! {
                    _ = stop.changed() => {},
                    _ = time::sleep(self.config.poll_interval) => {},
                }
                continue;
            }
            let idle = activity.is_idle_for(Duration::from_secs(self.config.idle_seconds));
            let kinds = handlers
                .iter()
                .filter(|(_, handler)| idle || handler.run_during_foreground_activity())
                .map(|(kind, _)| kind.clone())
                .collect::<Vec<_>>();
            if kinds.is_empty() {
                tokio::select! {
                    _ = stop.changed() => {},
                    _ = time::sleep(self.config.poll_interval) => {},
                }
                continue;
            }

            self.prune_terminal_jobs_if_due().await;
            let job = match self
                .store
                .claim(worker_id.clone(), kinds, self.config.lease_duration)
                .await
            {
                Ok(job) => job,
                Err(error) => {
                    tracing::error!(error = %error, "could not claim durable job");
                    tokio::time::sleep(self.config.poll_interval).await;
                    continue;
                }
            };
            let Some(job) = job else {
                tokio::select! {
                    _ = stop.changed() => {},
                    _ = time::sleep(self.config.poll_interval) => {},
                }
                continue;
            };
            let Some(handler) = handlers.get(&job.kind).cloned() else {
                continue;
            };
            self.execute(job, worker_id.clone(), handler, &mut stop)
                .await;
        }
    }

    async fn execute(
        &self,
        job: JobRecord,
        owner: String,
        handler: Arc<dyn JobHandler>,
        stop: &mut watch::Receiver<bool>,
    ) {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let context = JobContext {
            job: job.clone(),
            owner: owner.clone(),
            store: self.store.clone(),
            cancellation: cancel_rx,
        };
        let mut future = AssertUnwindSafe(handler.execute(context)).catch_unwind();
        let heartbeat_period = (self.config.lease_duration / 3).max(Duration::from_secs(1));
        let mut heartbeat = time::interval(heartbeat_period);
        heartbeat.tick().await;
        let outcome = loop {
            tokio::select! {
                result = &mut future => {
                    let outcome = result.unwrap_or_else(|_| JobOutcome::Failed {
                        message: "job handler panicked".to_string(),
                        retryable: true,
                    });
                    break Some(outcome);
                },
                _ = heartbeat.tick() => {
                    match self.store.heartbeat(job.id.clone(), owner.clone(), self.config.lease_duration).await {
                        Ok(Some(cancelled)) => {
                            if cancelled {
                                let _ = cancel_tx.send(true);
                            }
                        }
                        Ok(None) => {
                            let _ = cancel_tx.send(true);
                            tracing::warn!(job_id = %job.id, "job lease was lost; discarding late result");
                            break None;
                        }
                        Err(error) => {
                            tracing::error!(job_id = %job.id, error = %error, "job lease heartbeat failed");
                            let _ = cancel_tx.send(true);
                            break None;
                        }
                    }
                }
                _ = stop.changed() => {
                    if *stop.borrow() {
                        // Let the current atomic work unit finish during shutdown.
                        continue;
                    }
                }
            }
        };

        let Some(outcome) = outcome else {
            return;
        };
        let result = match outcome {
            JobOutcome::Succeeded(result) => self.store.finish(job.id.clone(), owner, result).await,
            JobOutcome::Failed { message, retryable } => self
                .store
                .fail(job.id.clone(), owner, message, retryable)
                .await
                .map(|_| true),
        };
        if let Err(error) = result {
            tracing::error!(job_id = %job.id, error = %error, "could not persist job result");
            return;
        }
        match self.store.get(job.id.clone()).await {
            Ok(Some(record))
                if matches!(
                    record.status,
                    JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
                ) =>
            {
                handler.after_terminal(&record).await;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(job_id = %job.id, error = %error, "could not load terminal job for cleanup")
            }
        }
    }

    async fn prune_terminal_jobs_if_due(&self) {
        let now = now_ms().max(0) as u64;
        let last = self.last_prune_ms.load(Ordering::Acquire);
        if now.saturating_sub(last) < PRUNE_INTERVAL.as_millis() as u64
            || self
                .last_prune_ms
                .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        if let Err(error) = self.store.prune_terminal(self.config.retention_days).await {
            tracing::error!(error = %error, "could not prune retained job history");
        }
    }
}

pub struct JobRuntime {
    stop: watch::Sender<bool>,
    tasks: JoinSet<()>,
}

impl JobRuntime {
    pub async fn shutdown(mut self, timeout: Duration) {
        let _ = self.stop.send(true);
        let drain = async {
            while let Some(result) = self.tasks.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(error = %error, "job dispatcher task exited unexpectedly");
                }
            }
        };
        if time::timeout(timeout, drain).await.is_err() {
            tracing::warn!(
                "job dispatcher drain deadline expired; active leases will recover after expiry"
            );
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
        }
    }
}

impl Drop for JobRuntime {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.tasks.abort_all();
    }
}

fn migrate(connection: &mut Connection) -> Result<(), JobError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(JobError::new(format!(
            "jobs database schema version {version} is newer than this binary supports"
        )));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE jobs (
            id TEXT PRIMARY KEY NOT NULL,
            queue TEXT NOT NULL,
            kind TEXT NOT NULL,
            parent_id TEXT REFERENCES jobs(id),
            dedupe_key TEXT,
            payload_json TEXT NOT NULL,
            policy_json TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('pending','running','retry_wait','succeeded','failed','cancelled')),
            priority INTEGER NOT NULL DEFAULT 0,
            attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
            max_attempts INTEGER NOT NULL CHECK(max_attempts BETWEEN 1 AND 100),
            lease_owner TEXT,
            lease_until_ms INTEGER,
            run_after_ms INTEGER NOT NULL,
            progress_json TEXT NOT NULL DEFAULT '{}',
            result_json TEXT,
            last_error TEXT,
            cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            finished_at_ms INTEGER
        );
        CREATE INDEX jobs_claim_idx ON jobs(state, run_after_ms, priority DESC, created_at_ms);
        CREATE INDEX jobs_queue_idx ON jobs(queue, state, created_at_ms DESC);
        CREATE UNIQUE INDEX jobs_active_dedupe_idx ON jobs(dedupe_key)
            WHERE dedupe_key IS NOT NULL AND state IN ('pending','running','retry_wait');
        CREATE TABLE job_dependencies (
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            depends_on_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            PRIMARY KEY(job_id, depends_on_id),
            CHECK(job_id <> depends_on_id)
        );
        CREATE INDEX job_dependencies_parent_idx ON job_dependencies(depends_on_id);
        CREATE TABLE job_events (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
            event TEXT NOT NULL,
            occurred_at_ms INTEGER NOT NULL,
            detail_json TEXT NOT NULL DEFAULT '{}'
        );
        CREATE INDEX job_events_job_idx ON job_events(job_id, sequence);
        CREATE TABLE job_schedules (
            id TEXT PRIMARY KEY NOT NULL,
            queue TEXT NOT NULL,
            kind TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            policy_json TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 0,
            max_attempts INTEGER NOT NULL CHECK(max_attempts BETWEEN 1 AND 100),
            interval_ms INTEGER NOT NULL CHECK(interval_ms > 0),
            next_run_ms INTEGER NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
            updated_at_ms INTEGER NOT NULL
        );
        PRAGMA user_version = 1;",
    )?;
    transaction.commit()?;
    Ok(())
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    let payload: String = row.get(5)?;
    let policy: String = row.get(6)?;
    let state: String = row.get(7)?;
    let lease_until: Option<i64> = row.get(12)?;
    let progress: String = row.get(14)?;
    let result: Option<String> = row.get(15)?;
    Ok(JobRecord {
        id: row.get(0)?,
        queue: row.get(1)?,
        kind: row.get(2)?,
        parent_id: row.get(3)?,
        dedupe_key: row.get(4)?,
        payload: serde_json::from_str(&payload).map_err(to_sql_conversion_error)?,
        policy_snapshot: serde_json::from_str(&policy).map_err(to_sql_conversion_error)?,
        status: JobStatus::parse(&state).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        priority: row.get(8)?,
        attempts: row.get(9)?,
        max_attempts: row.get(10)?,
        lease_owner: row.get(11)?,
        lease_until: lease_until.map(ms_system_time),
        run_after: ms_system_time(row.get(13)?),
        progress: serde_json::from_str(&progress).map_err(to_sql_conversion_error)?,
        result: result
            .map(|json| serde_json::from_str(&json).map_err(to_sql_conversion_error))
            .transpose()?,
        last_error: row.get(16)?,
        cancel_requested: row.get(17)?,
        created_at: ms_system_time(row.get(18)?),
        updated_at: ms_system_time(row.get(19)?),
    })
}

fn to_sql_conversion_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn validate_job_name(name: &str, value: &str) -> Result<(), JobError> {
    if value.is_empty()
        || value.len() > MAX_JOB_NAME_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(JobError::new(format!(
            "{name} must be 1..={MAX_JOB_NAME_LENGTH} ASCII letters, numbers, '.', '_' or '-'",
        )));
    }
    Ok(())
}

async fn run_blocking<T, F>(connection: Arc<Mutex<Connection>>, operation: F) -> Result<T, JobError>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, JobError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut connection)
    })
    .await
    .map_err(|error| JobError::new(format!("jobs database task failed: {error}")))?
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn ms_system_time(milliseconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(milliseconds.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn new_test_job(kind: &str) -> NewJob {
        NewJob::system("audit", kind, serde_json::json!({"scope": "test"}))
    }

    fn temporary_database() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("jobs.sqlite3");
        (directory, path)
    }

    #[tokio::test]
    async fn queued_jobs_survive_database_reopen() {
        let (_directory, path) = temporary_database();
        let store = JobStore::open(&path).expect("open job store");
        let id = store
            .enqueue(new_test_job("storage.audit"))
            .await
            .expect("enqueue job");
        drop(store);

        let reopened = JobStore::open(&path).expect("reopen job store");
        let job = reopened
            .get(id)
            .await
            .expect("load job")
            .expect("job exists");
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.queue, "audit");
        assert_eq!(job.kind, "storage.audit");
        assert_eq!(job.payload["scope"], "test");
    }

    #[tokio::test]
    async fn active_dedupe_key_returns_existing_job() {
        let (_directory, path) = temporary_database();
        let store = JobStore::open(path).expect("open job store");
        let mut first = new_test_job("storage.audit");
        first.dedupe_key = Some("audit:products".to_string());
        let first_id = store.enqueue(first).await.expect("enqueue first job");

        let mut duplicate = new_test_job("storage.audit");
        duplicate.dedupe_key = Some("audit:products".to_string());
        let duplicate_id = store
            .enqueue(duplicate)
            .await
            .expect("enqueue duplicate job");

        assert_eq!(first_id, duplicate_id);
        assert_eq!(
            store
                .list(Some("audit".to_string()), None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn dependencies_block_claim_until_parent_succeeds() {
        let (_directory, path) = temporary_database();
        let store = JobStore::open(path).expect("open job store");
        let parent_id = store
            .enqueue(new_test_job("storage.inventory"))
            .await
            .expect("enqueue parent");
        let mut child = new_test_job("storage.audit");
        child.dependencies.push(parent_id.clone());
        let child_id = store.enqueue(child).await.expect("enqueue child");

        let parent = store
            .claim(
                "worker-1".to_string(),
                vec!["storage.inventory".to_string(), "storage.audit".to_string()],
                Duration::from_secs(30),
            )
            .await
            .expect("claim parent")
            .expect("parent is ready");
        assert_eq!(parent.id, parent_id);
        assert!(
            store
                .claim(
                    "worker-1".to_string(),
                    vec!["storage.inventory".to_string(), "storage.audit".to_string()],
                    Duration::from_secs(30),
                )
                .await
                .expect("child remains blocked")
                .is_none()
        );

        assert!(
            store
                .finish(parent.id, "worker-1".to_string(), Value::Null)
                .await
                .unwrap()
        );
        let child = store
            .claim(
                "worker-1".to_string(),
                vec!["storage.audit".to_string()],
                Duration::from_secs(30),
            )
            .await
            .expect("claim child")
            .expect("child is ready");
        assert_eq!(child.id, child_id);
    }

    #[tokio::test]
    async fn retryable_failure_can_be_reclaimed_and_terminal_failure_is_recorded() {
        let (_directory, path) = temporary_database();
        let store = JobStore::open(path).expect("open job store");
        let id = store
            .enqueue(new_test_job("storage.audit"))
            .await
            .expect("enqueue job");
        let first = store
            .claim(
                "worker-1".to_string(),
                vec!["storage.audit".to_string()],
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.attempts, 1);
        assert_eq!(
            store
                .fail(
                    id.clone(),
                    "worker-1".to_string(),
                    "temporary storage error".to_string(),
                    true,
                )
                .await
                .unwrap(),
            Some(JobStatus::RetryWait)
        );
        {
            let connection = store
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            connection
                .execute("UPDATE jobs SET run_after_ms = 0 WHERE id = ?1", [&id])
                .unwrap();
        }
        let second = store
            .claim(
                "worker-2".to_string(),
                vec!["storage.audit".to_string()],
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.attempts, 2);
        assert_eq!(
            store
                .fail(
                    id.clone(),
                    "worker-2".to_string(),
                    "permanent error".to_string(),
                    false,
                )
                .await
                .unwrap(),
            Some(JobStatus::Failed)
        );
        assert_eq!(
            store.get(id).await.unwrap().unwrap().status,
            JobStatus::Failed
        );
    }

    #[tokio::test]
    async fn expired_lease_is_recovered_with_a_new_attempt() {
        let (_directory, path) = temporary_database();
        let store = JobStore::open(path).expect("open job store");
        let id = store
            .enqueue(new_test_job("storage.audit"))
            .await
            .expect("enqueue job");
        let first = store
            .claim(
                "worker-1".to_string(),
                vec!["storage.audit".to_string()],
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        {
            let connection = store
                .connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            connection
                .execute("UPDATE jobs SET lease_until_ms = 0 WHERE id = ?1", [&id])
                .unwrap();
        }
        let recovered = store
            .claim(
                "worker-2".to_string(),
                vec!["storage.audit".to_string()],
                Duration::from_secs(30),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.id, first.id);
        assert_eq!(recovered.attempts, 2);
        assert_eq!(recovered.lease_owner.as_deref(), Some("worker-2"));
    }

    #[tokio::test]
    async fn due_interval_schedule_materializes_once_while_job_is_active() {
        let (_directory, path) = temporary_database();
        let store = JobStore::open(path).expect("open job store");
        store
            .register_interval_schedule(IntervalSchedule {
                id: "inventory-products".to_string(),
                queue: "inventory".to_string(),
                kind: "storage.inventory".to_string(),
                payload: serde_json::json!({"namespace": "products"}),
                policy_snapshot: Value::Null,
                priority: 0,
                max_attempts: 3,
                interval: Duration::from_secs(60),
                run_immediately: true,
            })
            .await
            .expect("register schedule");

        assert_eq!(store.materialize_due_schedules().await.unwrap(), 1);
        assert_eq!(store.materialize_due_schedules().await.unwrap(), 0);
        assert_eq!(
            store
                .list(Some("inventory".to_string()), Some(JobStatus::Pending), 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    struct CheckpointHandler(Arc<AtomicUsize>);

    impl JobHandler for CheckpointHandler {
        fn execute<'a>(&'a self, context: JobContext) -> JobFuture<'a> {
            Box::pin(async move {
                let executions = self.0.fetch_add(1, Ordering::AcqRel) + 1;
                context
                    .checkpoint(serde_json::json!({"visited": executions}))
                    .await
                    .expect("checkpoint should persist");
                JobOutcome::Succeeded(serde_json::json!({"completed": true}))
            })
        }
    }

    #[tokio::test]
    async fn dispatcher_waits_for_foreground_activity_then_persists_result() {
        let (_directory, path) = temporary_database();
        let scheduler = JobScheduler::open(
            path,
            SchedulerConfig {
                concurrency: 1,
                idle_seconds: 0,
                poll_interval: Duration::from_millis(10),
                lease_duration: Duration::from_secs(3),
                daily_utc_window: None,
                retention_days: 30,
            },
        )
        .expect("open scheduler");
        let execution_count = Arc::new(AtomicUsize::new(0));
        scheduler
            .register_handler(
                "storage.audit",
                Arc::new(CheckpointHandler(execution_count.clone())),
            )
            .await
            .expect("register handler");
        let store = scheduler.store();
        let id = store
            .enqueue(new_test_job("storage.audit"))
            .await
            .expect("enqueue job");
        let activity = Arc::new(ActivityTracker::default());
        let foreground_request = activity.enter();
        let ready = Arc::new(AtomicBool::new(true));
        let runtime = scheduler.start(activity, ready);

        time::sleep(Duration::from_millis(50)).await;
        assert_eq!(execution_count.load(Ordering::Acquire), 0);
        drop(foreground_request);

        let completed = time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(job) = store.get(id.clone()).await.unwrap()
                    && job.status == JobStatus::Succeeded
                {
                    break job;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatcher should complete the job");
        assert_eq!(execution_count.load(Ordering::Acquire), 1);
        assert_eq!(completed.progress["visited"], 1);
        assert_eq!(completed.result.unwrap()["completed"], true);
        runtime.shutdown(Duration::from_secs(1)).await;
    }
}
