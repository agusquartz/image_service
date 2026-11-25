use crate::{
    auth::require_auth,
    config::ProcessingPolicy,
    error::ApiError,
    images::{
        ImageKey, IncomingImage, multipart::stage_multipart_image, reduction::reduce_to_webp,
        service,
    },
    state::AppState,
};
use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use job_scheduler::{
    JobContext, JobFuture, JobHandler, JobOutcome, JobRecord, JobStatus, NewJob,
    TargetEnqueueResult, TargetJobMode,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const IMAGE_JOB_QUEUE: &str = "image.processing";
const IMAGE_JOB_KIND: &str = "image.process.staged";

#[derive(Serialize)]
struct AcceptedImageJob {
    job_id: String,
    status: String,
    status_url: String,
    namespace: String,
    resource_id: String,
    slot: u8,
}

#[derive(Serialize)]
pub(crate) struct ImageJobStatus {
    job_id: String,
    status: String,
    attempts: u32,
    created_at_ms: u64,
    updated_at_ms: u64,
    namespace: String,
    resource_id: String,
    slot: u8,
    result: Option<Value>,
    error: Option<String>,
}

pub(crate) async fn create_image_job(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    submit_image_job(
        state,
        namespace,
        resource_id,
        slot,
        headers,
        multipart,
        false,
    )
    .await
}

pub(crate) async fn replace_image_job(
    State(state): State<AppState>,
    Path((namespace, resource_id, slot)): Path<(String, String, u8)>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    submit_image_job(
        state,
        namespace,
        resource_id,
        slot,
        headers,
        multipart,
        true,
    )
    .await
}

async fn submit_image_job(
    state: AppState,
    namespace: String,
    resource_id: String,
    slot: u8,
    headers: HeaderMap,
    multipart: Multipart,
    replace_existing: bool,
) -> Result<Response, ApiError> {
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    let key = ImageKey::new(namespace, resource_id, slot)?;
    let policy = state.processing_policy_for(&key.namespace);
    let idempotency_key = parse_idempotency_key(&headers)?;
    let target_lock = state.async_resource_lock(&key.namespace, &key.resource_id);
    let _target_guard = target_lock.lock().await;

    if slot > state.max_slots {
        if !replace_existing {
            return Err(ApiError::BadRequest(format!(
                "cannot create slot {slot}: IMAGE_MAX_SLOTS is {}",
                state.max_slots
            )));
        }
        match state.store.stat(&key).await {
            Ok(_) => {}
            Err(ApiError::NotFound(_)) => {
                return Err(ApiError::BadRequest(format!(
                    "cannot create slot {slot}: IMAGE_MAX_SLOTS is {}",
                    state.max_slots
                )));
            }
            Err(error) => return Err(error),
        }
    }
    if !replace_existing && idempotency_key.is_none() {
        state.store.ensure_absent(&key).await?;
    }

    let admission = state.admit_staging_upload()?;
    let stage_id = uuid::Uuid::new_v4().to_string();
    let writer = state.store.open_staged_writer(&stage_id).await?;
    let incoming = match tokio::time::timeout(
        std::time::Duration::from_secs(state.upload_read_timeout_seconds),
        stage_multipart_image(multipart, policy.max_input_bytes, writer),
    )
    .await
    {
        Ok(Ok(incoming)) => incoming,
        Ok(Err(error)) => {
            let _ = state.store.delete_staged(&stage_id).await;
            return Err(error);
        }
        Err(_) => {
            let _ = state.store.delete_staged(&stage_id).await;
            return Err(ApiError::RequestTimeout(
                "Upload body read timed out".to_string(),
            ));
        }
    };
    drop(admission);
    let target = target_dedupe_key(&key);
    let request_fingerprint = format!(
        "{}:{}/{}/{}:{}",
        if replace_existing { "PUT" } else { "POST" },
        key.namespace,
        key.resource_id,
        key.slot,
        incoming.content_sha256
    );
    if let Some(idempotency_key) = &idempotency_key {
        match state
            .job_scheduler
            .store()
            .get_by_idempotency_key(idempotency_key.clone())
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?
        {
            Some(existing)
                if existing
                    .payload
                    .get("request_fingerprint")
                    .and_then(Value::as_str)
                    == Some(request_fingerprint.as_str()) =>
            {
                let _ = state.store.delete_staged(&stage_id).await;
                return accepted_response(&state, existing.id, key).await;
            }
            Some(_) => {
                let _ = state.store.delete_staged(&stage_id).await;
                return Err(ApiError::Conflict(
                    "Idempotency-Key was already used for a different image request".to_string(),
                ));
            }
            None => {}
        }
    }
    if !replace_existing && let Err(error) = state.store.ensure_absent(&key).await {
        let _ = state.store.delete_staged(&stage_id).await;
        return Err(error);
    }
    let mut payload = json!({
        "namespace":key.namespace,
        "resource_id":key.resource_id,
        "slot":key.slot,
        "replace_existing":replace_existing,
        "stage_id":stage_id,
        "source_bytes":incoming.source_bytes,
        "mime":incoming.mime,
        "request_fingerprint":request_fingerprint,
    });
    if let Some(idempotency_key) = idempotency_key {
        payload["idempotency_key"] = Value::String(idempotency_key);
    }

    let mut job = NewJob::system(IMAGE_JOB_QUEUE, IMAGE_JOB_KIND, payload);
    job.dedupe_key = Some(target);
    job.policy_snapshot =
        serde_json::to_value(&policy).map_err(|error| ApiError::Internal(error.to_string()))?;
    job.priority = 50;
    job.max_attempts = 5;

    let enqueue = state
        .job_scheduler
        .store()
        .enqueue_targeted(
            job,
            if replace_existing {
                TargetJobMode::Supersede
            } else {
                TargetJobMode::CreateOnly
            },
            state.async_image_max_jobs,
            state.async_image_max_bytes,
            incoming.source_bytes as u64,
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()));

    let enqueue = match enqueue {
        Ok(result) => result,
        Err(error) => {
            let _ = state.store.delete_staged(&stage_id).await;
            return Err(error);
        }
    };

    let job_id = match enqueue {
        TargetEnqueueResult::Enqueued {
            id,
            superseded_stage_id,
            superseded_was_running,
        } => {
            if !superseded_was_running && let Some(previous_stage) = superseded_stage_id {
                let _ = state.store.delete_staged(&previous_stage).await;
            }
            id
        }
        TargetEnqueueResult::ExistingIdempotent(id) => {
            let _ = state.store.delete_staged(&stage_id).await;
            id
        }
        TargetEnqueueResult::ActiveConflict(_) => {
            let _ = state.store.delete_staged(&stage_id).await;
            return Err(ApiError::Conflict(
                "An image creation job already owns this slot".to_string(),
            ));
        }
        TargetEnqueueResult::IdempotencyConflict => {
            let _ = state.store.delete_staged(&stage_id).await;
            return Err(ApiError::Conflict(
                "Idempotency-Key was already used for a different image request".to_string(),
            ));
        }
        TargetEnqueueResult::CapacityExceeded => {
            let _ = state.store.delete_staged(&stage_id).await;
            return Err(ApiError::ServiceUnavailable(
                "Asynchronous image queue capacity is exhausted".to_string(),
            ));
        }
    };

    accepted_response(&state, job_id, key).await
}

async fn accepted_response(
    state: &AppState,
    job_id: String,
    key: ImageKey,
) -> Result<Response, ApiError> {
    let job = state
        .job_scheduler
        .store()
        .get(job_id.clone())
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?
        .ok_or_else(|| ApiError::Internal("Accepted image job was not persisted".into()))?;
    let status_url = format!("/api/image-jobs/{job_id}");
    let body = AcceptedImageJob {
        job_id,
        status: image_job_status(&job),
        status_url: status_url.clone(),
        namespace: key.namespace,
        resource_id: key.resource_id,
        slot: key.slot,
    };
    let mut response = (StatusCode::ACCEPTED, Json(body)).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&status_url)
            .map_err(|error| ApiError::Internal(error.to_string()))?,
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    Ok(response)
}

pub(crate) async fn get_image_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ImageJobStatus>, ApiError> {
    require_auth(&headers, state.write_auth, state.api_key.as_deref())?;
    let job = state
        .job_scheduler
        .store()
        .get(job_id)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?
        .filter(|job| job.kind == IMAGE_JOB_KIND)
        .ok_or_else(|| ApiError::NotFound("Image job not found".to_string()))?;
    let payload = &job.payload;
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
        .unwrap_or_default() as u8;
    let status = image_job_status(&job);
    Ok(Json(ImageJobStatus {
        job_id: job.id,
        status,
        attempts: job.attempts,
        created_at_ms: epoch_ms(job.created_at),
        updated_at_ms: epoch_ms(job.updated_at),
        namespace,
        resource_id,
        slot,
        result: job.result,
        error: job.last_error,
    }))
}

fn image_job_status(job: &JobRecord) -> String {
    if job.status == JobStatus::Cancelled
        && job
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("superseded"))
    {
        "superseded".to_string()
    } else {
        match job.status {
            JobStatus::Pending => "queued",
            JobStatus::Running => "processing",
            JobStatus::RetryWait => "retry_wait",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
        .to_string()
    }
}

pub(crate) async fn register_jobs(state: &AppState) -> Result<(), String> {
    state
        .job_scheduler
        .register_handler(
            IMAGE_JOB_KIND,
            std::sync::Arc::new(ImageProcessingHandler {
                state: state.clone(),
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    cleanup_unreferenced_staging(state).await
}

pub(crate) async fn cancel_target_for_sync_mutation(
    state: &AppState,
    key: &ImageKey,
) -> Result<(), ApiError> {
    let target = target_dedupe_key(key);
    let cancelled = state
        .job_scheduler
        .store()
        .cancel_target(target)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    if let Some((stage_id, was_running)) = cancelled
        && !was_running
    {
        state.store.delete_staged(&stage_id).await?;
    }
    Ok(())
}

pub(crate) async fn cancel_resource_for_sync_delete(
    state: &AppState,
    namespace: &str,
    resource_id: &str,
) -> Result<(), ApiError> {
    let store = state.job_scheduler.store();
    for status in [JobStatus::Pending, JobStatus::Running, JobStatus::RetryWait] {
        let jobs = store
            .list(Some(IMAGE_JOB_QUEUE.to_string()), Some(status), 1000)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        for job in jobs {
            if job.payload.get("namespace").and_then(Value::as_str) != Some(namespace)
                || job.payload.get("resource_id").and_then(Value::as_str) != Some(resource_id)
            {
                continue;
            }
            if let Some(target) = job.dedupe_key
                && let Some((stage_id, false)) = store
                    .cancel_target(target)
                    .await
                    .map_err(|error| ApiError::Internal(error.to_string()))?
            {
                state.store.delete_staged(&stage_id).await?;
            }
        }
    }
    Ok(())
}

struct ImageProcessingHandler {
    state: AppState,
}

impl JobHandler for ImageProcessingHandler {
    fn execute<'a>(&'a self, context: JobContext) -> JobFuture<'a> {
        Box::pin(async move {
            match self.process(&context).await {
                Ok(result) => JobOutcome::Succeeded(result),
                Err((message, retryable)) => JobOutcome::Failed { message, retryable },
            }
        })
    }

    fn run_during_foreground_activity(&self) -> bool {
        true
    }

    fn after_terminal<'a>(
        &'a self,
        job: &'a JobRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(stage_id) = job.payload.get("stage_id").and_then(Value::as_str)
                && let Err(error) = self.state.store.delete_staged(stage_id).await
            {
                tracing::warn!(job_id=%job.id, error=?error, "could not remove terminal image-job staging object");
            }
        })
    }
}

impl ImageProcessingHandler {
    async fn process(&self, context: &JobContext) -> Result<Value, (String, bool)> {
        let payload = &context.job.payload;
        let namespace = required_text(payload, "namespace")?;
        let resource_id = required_text(payload, "resource_id")?;
        let slot = payload
            .get("slot")
            .and_then(Value::as_u64)
            .filter(|slot| *slot <= u8::MAX as u64)
            .ok_or_else(|| ("Invalid image slot in job payload".to_string(), false))?
            as u8;
        let stage_id = required_text(payload, "stage_id")?;
        let replace_existing = payload
            .get("replace_existing")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let source_bytes = payload
            .get("source_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| ("Invalid source size in job payload".to_string(), false))?;
        let mime = payload
            .get("mime")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let key = ImageKey::new(namespace.clone(), resource_id.clone(), slot)
            .map_err(|error| (format!("{error:?}"), false))?;
        let policy: ProcessingPolicy = serde_json::from_value(context.job.policy_snapshot.clone())
            .map_err(|error| (format!("Invalid policy snapshot: {error}"), false))?;

        if job_cancelled(context, &self.state).await? {
            return Ok(json!({"status":"superseded"}));
        }
        let admission = self
            .state
            .admit_upload(policy.max_input_bytes)
            .map_err(|error| (format!("{error:?}"), true))?;
        let operation = self.state.store.begin_operation(&key).await;
        let source = self
            .state
            .store
            .read_staged(&stage_id)
            .await
            .map_err(|error| (format!("Could not read staged image: {error:?}"), true))?;
        if source.len() as u64 != source_bytes || source.len() > policy.max_input_bytes {
            return Err((
                "Staged image size does not match its accepted metadata".into(),
                false,
            ));
        }
        context
            .checkpoint(json!({"stage":"processing","source_bytes":source_bytes}))
            .await
            .map_err(|error| (error.to_string(), true))?;
        let output = reduce_to_webp(
            &self.state,
            self.state.image_processor.clone(),
            self.state.image_jobs.clone(),
            IncomingImage {
                bytes: source.into(),
                mime,
            },
            admission,
            &policy,
        )
        .await
        .map_err(classify_api_error)?;

        let target_lock = self
            .state
            .async_resource_lock(&key.namespace, &key.resource_id);
        let _target_guard = target_lock.lock().await;
        if job_cancelled(context, &self.state).await? {
            return Ok(json!({"status":"superseded"}));
        }
        context
            .checkpoint(json!({"stage":"publishing","output_bytes":output.len()}))
            .await
            .map_err(|error| (error.to_string(), true))?;
        let mutation_id = crate::storage_usage::begin_http_mutation(
            &self.state,
            &key,
            if replace_existing { "PUT" } else { "POST" },
        )
        .await
        .map_err(classify_api_error)?;
        let write_result = if replace_existing {
            self.state
                .store
                .write_with_token(&key, output.clone(), self.state.max_slots, &operation)
                .await
        } else {
            self.state
                .store
                .create_with_token(&key, output.clone(), self.state.max_slots, &operation)
                .await
                .map(|metadata| (true, metadata))
        };
        let (_, metadata) = match write_result {
            Ok(result) => result,
            Err(error) => {
                crate::storage_usage::queue_mutation_recovery(
                    &self.state,
                    mutation_id,
                    if replace_existing { "PUT" } else { "POST" },
                    &key.namespace,
                    &key.resource_id,
                    Some(key.slot),
                )
                .await;
                return Err(classify_api_error(error));
            }
        };
        crate::storage_usage::complete_http_mutation(
            &self.state,
            mutation_id,
            &key.namespace,
            &key.resource_id,
            Some(key.slot),
            Some(&policy),
            Some(&metadata),
        )
        .await
        .map_err(classify_api_error)?;

        let relative = service::relative_image_url(&key);
        Ok(json!({
            "status":"succeeded",
            "image_url":service::public_image_url(&self.state,&relative),
            "size_bytes":output.len(),
            "namespace":key.namespace,
            "resource_id":key.resource_id,
            "slot":key.slot
        }))
    }
}

async fn job_cancelled(context: &JobContext, state: &AppState) -> Result<bool, (String, bool)> {
    if context.cancellation_requested() {
        return Ok(true);
    }
    state
        .job_scheduler
        .store()
        .cancellation_requested(context.job.id.clone())
        .await
        .map_err(|error| (error.to_string(), true))
}

fn required_text(payload: &Value, field: &str) -> Result<String, (String, bool)> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| (format!("Invalid {field} in image job"), false))
}

fn classify_api_error(error: ApiError) -> (String, bool) {
    match error {
        ApiError::UnsupportedMediaType(message)
        | ApiError::UnprocessableEntity(message)
        | ApiError::BadRequest(message)
        | ApiError::Conflict(message)
        | ApiError::PreconditionFailed(message)
        | ApiError::PayloadTooLarge(message) => (message, false),
        ApiError::ChecksumMismatch => ("The upload checksum did not match".to_string(), false),
        ApiError::ServiceUnavailable(message) => (message, true),
        ApiError::Storage(_) | ApiError::Internal(_) => (
            "Temporary image processing or storage failure".to_string(),
            true,
        ),
        ApiError::NotFound(_) => ("The target image was not found".to_string(), false),
        ApiError::RequestTimeout(_) => ("Image processing timed out".to_string(), false),
        ApiError::Unauthorized => ("The job is not authorized".to_string(), false),
        ApiError::RateLimited(_) => ("The job was rate limited".to_string(), true),
        ApiError::NotImplemented(_) => {
            ("The requested operation is unsupported".to_string(), false)
        }
    }
}

fn target_dedupe_key(key: &ImageKey) -> String {
    format!(
        "async-image:{}/{}/{}",
        key.namespace, key.resource_id, key.slot
    )
}

fn parse_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::BadRequest("Idempotency-Key must be valid ASCII".into()))?;
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(ApiError::BadRequest(
            "Idempotency-Key must contain 1 to 128 visible ASCII characters".into(),
        ));
    }
    Ok(Some(value.to_owned()))
}

fn epoch_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

async fn cleanup_unreferenced_staging(state: &AppState) -> Result<(), String> {
    let store = state.job_scheduler.store();
    let mut referenced = std::collections::HashSet::new();
    for status in [JobStatus::Pending, JobStatus::Running, JobStatus::RetryWait] {
        for job in store
            .list(Some(IMAGE_JOB_QUEUE.to_string()), Some(status), 1000)
            .await
            .map_err(|error| error.to_string())?
        {
            if let Some(stage_id) = job.payload.get("stage_id").and_then(Value::as_str) {
                referenced.insert(stage_id.to_owned());
            }
        }
    }
    for stage_id in state
        .store
        .list_staged_ids()
        .await
        .map_err(|error| format!("Could not list staged uploads: {error:?}"))?
    {
        if !referenced.contains(&stage_id) {
            state
                .store
                .delete_staged(&stage_id)
                .await
                .map_err(|error| format!("Could not remove orphaned staged upload: {error:?}"))?;
        }
    }
    Ok(())
}
