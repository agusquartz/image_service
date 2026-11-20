#[cfg(feature = "images")]
use crate::storage_usage::StorageCatalog;
#[cfg(feature = "images")]
use crate::{config::UPLOAD_BUDGET_UNIT_BYTES, error::ApiError, images::ImageStore};
use crate::{
    config::{
        AppConfig, AuthRequirement, ProcessingPolicy, ProcessingProfile,
        max_configured_input_bytes, resolve_processing_policy,
    },
    observability::HttpMetrics,
    rate_limit::RateLimiter,
};

use job_scheduler::{ActivityTracker, DailyUtcWindow, JobScheduler, SchedulerConfig};
use opendal::Operator;
#[cfg(feature = "images")]
use processor_contract::Processor;

#[cfg(feature = "images")]
use std::hash::{Hash, Hasher};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(feature = "images")]
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

/// Keeps admission capacity reserved while the compressed source remains in memory.
#[cfg(feature = "images")]
pub(crate) struct UploadAdmission {
    _slot: OwnedSemaphorePermit,
    _body_budget: OwnedSemaphorePermit,
}

#[cfg(feature = "images")]
const STAGED_UPLOAD_BUFFER_RESERVATION_BYTES: usize = 5 * 1024 * 1024;

#[cfg(feature = "images")]
pub(crate) struct BackgroundReadAdmission {
    _body_budget: OwnedSemaphorePermit,
}

#[derive(Clone, Default)]
pub struct ServiceLifecycle {
    ready: Arc<AtomicBool>,
}

impl ServiceLifecycle {
    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub(crate) fn ready_flag(&self) -> Arc<AtomicBool> {
        self.ready.clone()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub lifecycle: ServiceLifecycle,

    /// Durable internal job queue and bounded in-process dispatcher.
    pub job_scheduler: JobScheduler,

    /// Queue and staging limits for asynchronous image processing.
    pub async_image_max_jobs: usize,
    pub async_image_max_bytes: u64,

    /// Persistent object usage snapshot; disabled unless explicitly enabled.
    #[cfg(feature = "images")]
    pub storage_catalog: Option<StorageCatalog>,

    pub storage_usage_enabled: bool,

    pub storage_inventory_interval_seconds: u64,

    pub background_idle_seconds: u64,

    /// Tracks foreground HTTP activity so background work waits for quiet periods.
    pub activity: Arc<ActivityTracker>,

    /// Shared storage operator used by readiness and optional image routes.
    pub storage_operator: Operator,

    /// Backend-independent image repository.
    #[cfg(feature = "images")]
    pub store: ImageStore,

    #[cfg(feature = "images")]
    pub image_jobs: Arc<Semaphore>,

    #[cfg(feature = "images")]
    pub image_processor: Arc<dyn Processor>,

    #[cfg(feature = "images")]
    upload_slots: Arc<Semaphore>,
    #[cfg(feature = "images")]
    upload_body_budget: Arc<Semaphore>,
    #[cfg(feature = "images")]
    async_resource_locks: Arc<Vec<Mutex<()>>>,
    #[cfg(feature = "images")]
    pub upload_read_timeout_seconds: u64,

    pub max_source_bytes: usize,

    /// Largest input limit across the global policy and configured profiles.
    /// Axum uses this for its outer request-body limit; the selected policy
    /// still enforces the narrower per-namespace image limit.
    pub max_request_source_bytes: usize,

    pub max_output_bytes: usize,

    pub output_width: u32,

    pub output_height: u32,

    pub processing_profiles: HashMap<String, ProcessingProfile>,

    pub namespace_profiles: HashMap<String, String>,

    pub max_slots: u8,

    pub api_key: Option<String>,

    pub read_auth: AuthRequirement,

    pub write_auth: AuthRequirement,

    pub rate_limiter: Arc<RateLimiter>,

    pub metrics: Arc<HttpMetrics>,

    pub metrics_enabled: bool,

    pub metrics_auth: bool,

    pub presign_ttl_seconds: u64,

    pub cache_control: String,

    pub public_base_url: Option<String>,
}

impl AppState {
    /// Creates the shared application state from runtime configuration.
    pub fn from_config(
        config: &AppConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let operator = Operator::via_iter(&config.storage.scheme, config.storage.options.clone())?;
        let daily_utc_window = config
            .background_window_utc
            .as_deref()
            .map(DailyUtcWindow::parse)
            .transpose()?;
        let job_scheduler = JobScheduler::open(
            &config.job_database_path,
            SchedulerConfig {
                concurrency: config.background_job_concurrency,
                idle_seconds: config.background_idle_seconds,
                daily_utc_window,
                retention_days: config.job_retention_days,
                ..SchedulerConfig::default()
            },
        )?;
        #[cfg(feature = "images")]
        let storage_catalog = if config.storage_usage_enabled {
            Some(
                StorageCatalog::open(&config.storage_catalog_path)
                    .map_err(std::io::Error::other)?,
            )
        } else {
            None
        };

        Ok(Self {
            lifecycle: ServiceLifecycle::default(),
            job_scheduler,
            async_image_max_jobs: config.async_image_max_jobs,
            async_image_max_bytes: config.async_image_max_bytes,
            #[cfg(feature = "images")]
            storage_catalog,
            storage_usage_enabled: config.storage_usage_enabled,
            storage_inventory_interval_seconds: config.storage_inventory_interval_seconds,
            background_idle_seconds: config.background_idle_seconds,
            activity: Arc::new(ActivityTracker::default()),

            storage_operator: operator.clone(),

            #[cfg(feature = "images")]
            store: ImageStore::new(operator),

            #[cfg(feature = "images")]
            image_jobs: Arc::new(Semaphore::new(config.max_concurrency)),

            #[cfg(feature = "images")]
            image_processor: Arc::new(processor_image::ImageProcessor),

            #[cfg(feature = "images")]
            upload_slots: Arc::new(Semaphore::new(config.max_uploads_in_flight)),
            #[cfg(feature = "images")]
            upload_body_budget: Arc::new(Semaphore::new(
                config.upload_memory_budget_bytes / UPLOAD_BUDGET_UNIT_BYTES,
            )),
            #[cfg(feature = "images")]
            async_resource_locks: Arc::new((0..256).map(|_| Mutex::new(())).collect()),
            #[cfg(feature = "images")]
            upload_read_timeout_seconds: config.upload_read_timeout_seconds,

            max_source_bytes: config.max_source_bytes,

            max_request_source_bytes: max_configured_input_bytes(
                config.max_source_bytes,
                &config.processing_profiles,
                &config.namespace_profiles,
            ),

            max_output_bytes: config.max_output_bytes,

            output_width: config.output_width,

            output_height: config.output_height,

            processing_profiles: config.processing_profiles.clone(),

            namespace_profiles: config.namespace_profiles.clone(),

            max_slots: config.max_slots,

            api_key: config.api_key.clone(),

            read_auth: config.read_auth,

            write_auth: config.write_auth,

            rate_limiter: Arc::new(RateLimiter::new(config.rate_limit)),

            metrics: Arc::new(HttpMetrics::default()),

            metrics_enabled: config.metrics_enabled,

            metrics_auth: config.metrics_auth,

            presign_ttl_seconds: config.presign_ttl_seconds,

            cache_control: config.cache_control.clone(),

            public_base_url: config.public_base_url.clone(),
        })
    }

    pub fn processing_policy_for(&self, namespace: &str) -> ProcessingPolicy {
        resolve_processing_policy(
            namespace,
            self.max_source_bytes,
            self.max_output_bytes,
            self.output_width,
            self.output_height,
            &self.processing_profiles,
            &self.namespace_profiles,
        )
    }

    #[cfg(feature = "images")]
    pub(crate) fn admit_upload(&self, max_input_bytes: usize) -> Result<UploadAdmission, ApiError> {
        reserve_upload(
            self.upload_slots.clone(),
            self.upload_body_budget.clone(),
            max_input_bytes,
        )
    }

    #[cfg(feature = "images")]
    pub(crate) fn admit_staging_upload(&self) -> Result<UploadAdmission, ApiError> {
        reserve_upload(
            self.upload_slots.clone(),
            self.upload_body_budget.clone(),
            STAGED_UPLOAD_BUFFER_RESERVATION_BYTES,
        )
    }

    #[cfg(feature = "images")]
    pub(crate) fn async_resource_lock(&self, namespace: &str, resource_id: &str) -> &Mutex<()> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        namespace.hash(&mut hasher);
        resource_id.hash(&mut hasher);
        let index = hasher.finish() as usize % self.async_resource_locks.len();
        &self.async_resource_locks[index]
    }

    #[cfg(feature = "images")]
    pub(crate) fn admit_background_read(
        &self,
        bytes: usize,
    ) -> Result<BackgroundReadAdmission, ApiError> {
        let units = u32::try_from(bytes.div_ceil(UPLOAD_BUDGET_UNIT_BYTES))
            .map_err(|_| ApiError::Internal("Background read size exceeds budget units".into()))?;
        let body_budget = self
            .upload_body_budget
            .clone()
            .try_acquire_many_owned(units)
            .map_err(|_| {
                ApiError::ServiceUnavailable("Background image memory budget is busy".into())
            })?;
        Ok(BackgroundReadAdmission {
            _body_budget: body_budget,
        })
    }
}

#[cfg(feature = "images")]
fn reserve_upload(
    upload_slots: Arc<Semaphore>,
    upload_body_budget: Arc<Semaphore>,
    max_input_bytes: usize,
) -> Result<UploadAdmission, ApiError> {
    let slot = upload_slots
        .try_acquire_owned()
        .map_err(|_| ApiError::ServiceUnavailable("Upload capacity exhausted".to_string()))?;
    let units = u32::try_from(max_input_bytes.div_ceil(UPLOAD_BUDGET_UNIT_BYTES))
        .map_err(|_| ApiError::Internal("Upload input limit exceeds budget units".into()))?;
    let body_budget = upload_body_budget
        .try_acquire_many_owned(units)
        .map_err(|_| ApiError::ServiceUnavailable("Upload memory budget exhausted".into()))?;
    Ok(UploadAdmission {
        _slot: slot,
        _body_budget: body_budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_readiness_can_be_changed_across_clones() {
        let lifecycle = ServiceLifecycle::default();
        let server_lifecycle = lifecycle.clone();

        assert!(!lifecycle.is_ready());
        server_lifecycle.set_ready(true);
        assert!(lifecycle.is_ready());
        lifecycle.set_ready(false);
        assert!(!server_lifecycle.is_ready());
    }

    #[test]
    fn admission_releases_count_after_capacity_rejection() {
        let slots = Arc::new(Semaphore::new(2));
        let budget = Arc::new(Semaphore::new(1));
        let first = reserve_upload(slots.clone(), budget.clone(), 1).unwrap();
        assert!(matches!(
            reserve_upload(slots.clone(), budget.clone(), 1),
            Err(ApiError::ServiceUnavailable(_))
        ));
        assert_eq!(slots.available_permits(), 1);
        drop(first);
        assert!(reserve_upload(slots, budget, 1).is_ok());
    }

    #[test]
    fn admission_reserves_effective_limit_until_dropped() {
        let slots = Arc::new(Semaphore::new(3));
        let budget = Arc::new(Semaphore::new(3));
        let first =
            reserve_upload(slots.clone(), budget.clone(), UPLOAD_BUDGET_UNIT_BYTES + 1).unwrap();
        assert_eq!(budget.available_permits(), 1);
        assert!(matches!(
            reserve_upload(slots.clone(), budget.clone(), UPLOAD_BUDGET_UNIT_BYTES + 1),
            Err(ApiError::ServiceUnavailable(_))
        ));
        drop(first);
        assert!(reserve_upload(slots, budget, UPLOAD_BUDGET_UNIT_BYTES + 1).is_ok());
    }
}
