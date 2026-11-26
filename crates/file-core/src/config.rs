use std::{collections::HashMap, env, io, path::PathBuf};

mod cache;
mod jobs;
mod observability;
mod parsing;
mod processing;
mod security;
mod storage;
mod transfers;

pub use observability::RateLimitConfig;
use parsing::{parse_bool_env, parse_bounded_u32, parse_bounded_u64, parse_bounded_usize};
#[cfg(feature = "images")]
pub(crate) use processing::estimate_processing_memory_bytes;
pub(crate) use processing::{
    ProcessingPolicy, ProcessingProfile, max_configured_input_bytes, resolve_processing_policy,
};
pub use security::AuthRequirement;
pub use storage::StorageConfig;

/// Default maximum accepted source image size.
///
/// Images larger than this value are rejected before processing.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 15 * 1024 * 1024;

/// Default maximum encoded output size.
///
/// Every successfully processed image must fit within this limit.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 100 * 1024;
pub const DEFAULT_MAX_DECODE_DIMENSION: u32 = 12_000;
pub const DEFAULT_MAX_DECODE_ALLOCATION_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_CONCURRENCY: usize = 2;

/// Default freshness lifetime for image responses, in seconds.
pub const DEFAULT_IMAGE_CACHE_TTL_SECONDS: u64 = 60;

/// Maximum HTTP request overhead for multipart headers and boundaries.
pub const MULTIPART_OVERHEAD_BYTES: usize = 1024 * 1024;
pub const UPLOAD_BUDGET_UNIT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_UPLOADS_IN_FLIGHT: usize = 4;
/// Maximum source bytes kept in RAM while receiving one synchronous upload.
pub const DEFAULT_UPLOAD_MEMORY_THRESHOLD_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_MAX_DOWNLOADS_IN_FLIGHT: usize = 64;
pub const DEFAULT_DOWNLOAD_STREAM_CHUNK_BYTES: usize = 256 * 1024;
pub const DEFAULT_UPLOAD_READ_TIMEOUT_SECONDS: u64 = 120;
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECONDS: u64 = 25;

/// Minimum valid image slot.
pub const MIN_SLOT: u8 = 1;

/// Default maximum number of slots available to each resource.
pub const DEFAULT_MAX_SLOTS: u8 = 5;

/// Slot path parameters and storage keys use an unsigned 8-bit slot number.
pub const MAX_SLOT_NUMBER: u8 = u8::MAX;

/// Maximum source image dimensions accepted for converted output.
pub const DEFAULT_OUTPUT_WIDTH: u32 = 1024;
pub const DEFAULT_OUTPUT_HEIGHT: u32 = 1024;

/// Maximum presigned URL lifetime supported by the S3 API.
pub const MAX_PRESIGN_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Runtime application configuration.
///
/// Environment-specific values live here instead of being scattered
/// through handlers or services.
#[derive(Clone)]
pub struct AppConfig {
    /// HTTP address on which the image service will listen.
    pub bind_address: String,

    pub storage: StorageConfig,

    /// Maximum number of CPU-intensive image processing jobs allowed
    /// to execute concurrently.
    pub max_concurrency: usize,

    /// Maximum decoded source width or height and configured output dimension.
    pub max_decode_dimension: u32,

    /// Decoder's per-image allocation guard.
    pub max_decode_allocation_bytes: u64,

    /// Shared weighted budget for estimated image-processing working memory.
    pub processing_memory_budget_bytes: u64,

    /// Requests admitted to read or process an uploaded image at once.
    pub max_uploads_in_flight: usize,

    /// Budget reserved for compressed upload bodies while in flight.
    pub upload_memory_budget_bytes: usize,

    /// Per-upload RAM threshold before synchronous input spills to storage staging.
    pub upload_memory_threshold_bytes: usize,

    pub upload_read_timeout_seconds: u64,

    /// Whether clients may create opt-in Tus resumable upload sessions.
    pub resumable_uploads_enabled: bool,

    /// Lifetime for incomplete resumable upload sessions.
    pub resumable_upload_ttl_seconds: u64,

    /// Maximum body size accepted for one resumable upload chunk.
    pub resumable_upload_max_chunk_bytes: usize,

    /// Maximum active streamed image responses.
    pub max_downloads_in_flight: usize,

    /// Maximum target chunk size used by streamed image responses.
    pub download_stream_chunk_bytes: usize,

    /// Maximum time to drain active HTTP requests after SIGINT or SIGTERM.
    pub shutdown_timeout_seconds: u64,

    /// Persistent SQLite database for internal system jobs.
    pub job_database_path: PathBuf,

    /// Maximum concurrent background jobs. User requests remain the priority.
    pub background_job_concurrency: usize,

    /// Quiet period with no active HTTP operations before background work can start.
    pub background_idle_seconds: u64,

    /// Optional daily UTC window for background work, formatted as HH:MM-HH:MM.
    pub background_window_utc: Option<String>,

    /// Retention for completed internal jobs and their event history.
    pub job_retention_days: u64,

    /// Maximum active asynchronous image jobs.
    pub async_image_max_jobs: usize,

    /// Maximum source bytes retained by active asynchronous image jobs.
    pub async_image_max_bytes: u64,

    /// Whether to expose authenticated storage usage and run inventory scans.
    pub storage_usage_enabled: bool,

    /// Persistent catalog containing the latest completed storage inventory.
    pub storage_catalog_path: PathBuf,

    /// Interval between full storage inventory reconciliations.
    pub storage_inventory_interval_seconds: u64,

    /// Maximum uploaded image payload size, in bytes.
    pub max_source_bytes: usize,

    /// Maximum encoded WebP size, in bytes.
    pub max_output_bytes: usize,

    /// Exact width of processed images in pixels.
    pub output_width: u32,

    /// Exact height of processed images in pixels.
    pub output_height: u32,

    /// Named processing profiles; individual fields override global values.
    pub processing_profiles: HashMap<String, ProcessingProfile>,

    /// Namespace-to-profile assignments.
    pub namespace_profiles: HashMap<String, String>,

    /// Maximum valid image slot for each resource.
    pub max_slots: u8,

    /// Bearer token used by authentication policies.
    pub api_key: Option<String>,

    /// Whether read operations require the configured bearer token.
    pub read_auth: AuthRequirement,

    /// Whether write operations require the configured bearer token.
    pub write_auth: AuthRequirement,

    /// Read and write request budgets per client and rate-limit window.
    pub rate_limit: RateLimitConfig,

    /// Whether to expose the Prometheus metrics endpoint.
    pub metrics_enabled: bool,

    /// Whether requests to the Prometheus metrics endpoint require the API key.
    pub metrics_auth: bool,

    /// Lifetime for presigned image-read URLs.
    pub presign_ttl_seconds: u64,

    /// Cache-Control value applied to image GET and HEAD responses.
    pub cache_control: String,

    /// Optional externally visible base URL.
    ///
    /// Example:
    ///
    /// `https://images.example.com`
    ///
    /// When omitted, API responses contain relative URLs.
    pub public_base_url: Option<String>,
}

impl AppConfig {
    /// Builds the application configuration from environment variables.
    ///
    /// Supported variables:
    ///
    /// - `IMAGE_BIND`
    /// - `IMAGE_STORAGE_SCHEME`
    /// - `IMAGE_STORAGE_*` backend-specific options
    /// - `IMAGE_MAX_CONCURRENCY`
    /// - `IMAGE_MAX_DECODE_DIMENSION`
    /// - `IMAGE_MAX_DECODE_ALLOCATION_BYTES`
    /// - `IMAGE_PROCESSING_MEMORY_BUDGET_BYTES`
    /// - `IMAGE_MAX_UPLOADS_IN_FLIGHT`
    /// - `IMAGE_UPLOAD_MEMORY_BUDGET_BYTES`
    /// - `IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES`
    /// - `IMAGE_UPLOAD_READ_TIMEOUT_SECONDS`
    /// - `IMAGE_RESUMABLE_UPLOAD_MAX_CHUNK_BYTES`
    /// - `IMAGE_MAX_DOWNLOADS_IN_FLIGHT`
    /// - `IMAGE_DOWNLOAD_STREAM_CHUNK_BYTES`
    /// - `IMAGE_SHUTDOWN_TIMEOUT_SECONDS`
    /// - `IMAGE_JOB_DB_PATH`
    /// - `IMAGE_BACKGROUND_JOB_CONCURRENCY`
    /// - `IMAGE_BACKGROUND_IDLE_SECONDS`
    /// - `IMAGE_BACKGROUND_WINDOW_UTC`
    /// - `IMAGE_MAX_INPUT_BYTES`
    /// - `IMAGE_MAX_OUTPUT_BYTES`
    /// - `IMAGE_OUTPUT_WIDTH`
    /// - `IMAGE_OUTPUT_HEIGHT`
    /// - `IMAGE_PROCESSING_PROFILES` (JSON object)
    /// - `IMAGE_NAMESPACE_PROFILES` (JSON object)
    /// - `IMAGE_MAX_SLOTS`
    /// - `IMAGE_API_KEY`
    /// - `IMAGE_READ_AUTH`
    /// - `IMAGE_WRITE_AUTH`
    /// - `IMAGE_RATE_LIMIT_READ_REQUESTS`
    /// - `IMAGE_RATE_LIMIT_WRITE_REQUESTS`
    /// - `IMAGE_RATE_LIMIT_WINDOW_SECONDS`
    /// - `IMAGE_METRICS_ENABLED`
    /// - `IMAGE_PRESIGN_TTL_SECONDS`
    /// - `IMAGE_CACHE_CONTROL`
    /// - `IMAGE_CACHE_TTL_SECONDS`
    /// - `IMAGE_PUBLIC_BASE_URL`
    pub fn from_env() -> io::Result<Self> {
        let bind_address = env::var("IMAGE_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_string());

        let storage = storage::load()?;
        let processing = processing::load()?;
        let transfer = transfers::load(processing.largest_input)?;
        let jobs = jobs::load()?;
        let security = security::load(jobs.storage_usage_enabled)?;
        let observability = observability::load(security.api_key.as_deref())?;
        let cache = cache::load(security.read_auth)?;

        Ok(Self {
            bind_address,
            storage,
            max_concurrency: processing.max_concurrency,
            max_decode_dimension: processing.max_decode_dimension,
            max_decode_allocation_bytes: processing.max_decode_allocation_bytes,
            processing_memory_budget_bytes: processing.processing_memory_budget_bytes,
            max_uploads_in_flight: transfer.max_uploads_in_flight,
            upload_memory_budget_bytes: transfer.upload_memory_budget_bytes,
            upload_memory_threshold_bytes: transfer.upload_memory_threshold_bytes,
            upload_read_timeout_seconds: transfer.upload_read_timeout_seconds,
            resumable_uploads_enabled: transfer.resumable_uploads_enabled,
            resumable_upload_ttl_seconds: transfer.resumable_upload_ttl_seconds,
            resumable_upload_max_chunk_bytes: transfer.resumable_upload_max_chunk_bytes,
            max_downloads_in_flight: transfer.max_downloads_in_flight,
            download_stream_chunk_bytes: transfer.download_stream_chunk_bytes,
            shutdown_timeout_seconds: transfer.shutdown_timeout_seconds,
            job_database_path: jobs.database_path,
            background_job_concurrency: jobs.background_concurrency,
            background_idle_seconds: jobs.background_idle_seconds,
            background_window_utc: jobs.background_window_utc,
            job_retention_days: jobs.retention_days,
            async_image_max_jobs: jobs.async_image_max_jobs,
            async_image_max_bytes: jobs.async_image_max_bytes,
            storage_usage_enabled: jobs.storage_usage_enabled,
            storage_catalog_path: jobs.storage_catalog_path,
            storage_inventory_interval_seconds: jobs.storage_inventory_interval_seconds,
            max_source_bytes: processing.max_source_bytes,
            max_output_bytes: processing.max_output_bytes,
            output_width: processing.output_width,
            output_height: processing.output_height,
            processing_profiles: processing.processing_profiles,
            namespace_profiles: processing.namespace_profiles,
            max_slots: security.max_slots,
            api_key: security.api_key,
            read_auth: security.read_auth,
            write_auth: security.write_auth,
            rate_limit: observability.rate_limit,
            metrics_enabled: observability.metrics_enabled,
            metrics_auth: observability.metrics_auth,
            presign_ttl_seconds: security.presign_ttl_seconds,
            cache_control: cache.cache_control,
            public_base_url: cache.public_base_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_policies_accept_required_and_explicit_none() {
        assert_eq!(
            AuthRequirement::parse_value("IMAGE_READ_AUTH", "required").unwrap(),
            AuthRequirement::Required
        );
        assert_eq!(
            AuthRequirement::parse_value("IMAGE_WRITE_AUTH", "none").unwrap(),
            AuthRequirement::None
        );
        assert!(AuthRequirement::parse_value("IMAGE_READ_AUTH", "public").is_err());
        assert!(AuthRequirement::Required.is_required());
        assert!(!AuthRequirement::None.is_required());
    }

    #[test]
    fn production_rate_limit_defaults_are_independent_and_bounded() {
        assert_eq!(observability::DEFAULT_READ_RATE_LIMIT_REQUESTS, 600);
        assert_eq!(observability::DEFAULT_WRITE_RATE_LIMIT_REQUESTS, 60);
        assert_eq!(observability::DEFAULT_RATE_LIMIT_WINDOW_SECONDS, 60);
    }

    #[test]
    fn shutdown_timeout_defaults_to_25_seconds_and_is_bounded() {
        assert_eq!(DEFAULT_SHUTDOWN_TIMEOUT_SECONDS, 25);
        assert_eq!(
            parsing::validate_bounded_u64("IMAGE_SHUTDOWN_TIMEOUT_SECONDS", 25, 1, 3600).unwrap(),
            25
        );
        assert!(
            parsing::validate_bounded_u64("IMAGE_SHUTDOWN_TIMEOUT_SECONDS", 0, 1, 3600).is_err()
        );
        assert!(
            parsing::validate_bounded_u64("IMAGE_SHUTDOWN_TIMEOUT_SECONDS", 3601, 1, 3600).is_err()
        );
    }

    #[test]
    fn upload_memory_threshold_has_an_eight_mib_default_and_requires_positive_bytes() {
        assert_eq!(DEFAULT_UPLOAD_MEMORY_THRESHOLD_BYTES, 8 * 1024 * 1024);
        assert_eq!(
            parsing::validate_bounded_usize(
                "IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES",
                DEFAULT_UPLOAD_MEMORY_THRESHOLD_BYTES,
                1,
                usize::MAX,
            )
            .unwrap(),
            8 * 1024 * 1024
        );
        assert!(
            parsing::validate_bounded_usize(
                "IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES",
                0,
                1,
                usize::MAX
            )
            .is_err()
        );
    }

    #[test]
    fn metrics_auth_defaults_to_required_and_only_requires_key_when_metrics_are_enabled() {
        assert!(parsing::parse_bool_value("IMAGE_METRICS_AUTH", None, true).unwrap());
        assert!(observability::validate_metrics_auth(true, true, None).is_err());
        assert!(observability::validate_metrics_auth(false, true, None).is_ok());
        assert!(observability::validate_metrics_auth(true, false, None).is_ok());
        assert!(observability::validate_metrics_auth(true, true, Some("secret")).is_ok());
    }

    #[test]
    fn storage_usage_requires_a_key_only_when_enabled() {
        assert!(security::validate_storage_usage_auth(false, None).is_ok());
        assert!(security::validate_storage_usage_auth(true, None).is_err());
        assert!(security::validate_storage_usage_auth(true, Some("secret")).is_ok());
    }

    #[test]
    fn unassigned_namespaces_keep_the_existing_global_processing_policy() {
        let policy = resolve_processing_policy(
            "products",
            DEFAULT_MAX_SOURCE_BYTES,
            100_000,
            1024,
            1024,
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(
            policy,
            ProcessingPolicy {
                profile_name: "default".to_string(),
                max_input_bytes: DEFAULT_MAX_SOURCE_BYTES,
                max_output_bytes: 100_000,
                output_width: 1024,
                output_height: 1024,
            }
        );
    }

    #[test]
    fn profile_overrides_only_explicit_fields_and_only_for_mapped_namespaces() {
        let profiles = HashMap::from([(
            "avatar".to_string(),
            ProcessingProfile {
                max_input_bytes: None,
                max_output_bytes: Some(50_000),
                output_width: Some(512),
                output_height: Some(512),
            },
        )]);
        let assignments = HashMap::from([("users".to_string(), "avatar".to_string())]);

        let avatar = resolve_processing_policy(
            "users",
            17 * 1024 * 1024,
            100_000,
            1024,
            768,
            &profiles,
            &assignments,
        );
        assert_eq!(avatar.profile_name, "avatar");
        assert_eq!(avatar.max_input_bytes, 17 * 1024 * 1024);
        assert_eq!(avatar.max_output_bytes, 50_000);
        assert_eq!((avatar.output_width, avatar.output_height), (512, 512));

        let product = resolve_processing_policy(
            "products",
            17 * 1024 * 1024,
            100_000,
            1024,
            768,
            &profiles,
            &assignments,
        );
        assert_eq!(product.profile_name, "default");
        assert_eq!(product.max_input_bytes, 17 * 1024 * 1024);
        assert_eq!(product.max_output_bytes, 100_000);
        assert_eq!((product.output_width, product.output_height), (1024, 768));
    }

    #[test]
    fn partial_profile_fields_inherit_global_values() {
        let profiles = HashMap::from([(
            "compact".to_string(),
            ProcessingProfile {
                max_input_bytes: Some(18 * 1024 * 1024),
                max_output_bytes: Some(40_000),
                output_width: None,
                output_height: None,
            },
        )]);
        let assignments = HashMap::from([("products".to_string(), "compact".to_string())]);

        let policy = resolve_processing_policy(
            "products",
            17 * 1024 * 1024,
            100_000,
            1024,
            768,
            &profiles,
            &assignments,
        );

        assert_eq!(policy.max_input_bytes, 18 * 1024 * 1024);
        assert_eq!(policy.max_output_bytes, 40_000);
        assert_eq!((policy.output_width, policy.output_height), (1024, 768));
    }

    #[test]
    fn rejects_invalid_profile_dimensions_and_unknown_assignments() {
        let partial_dimensions = HashMap::from([(
            "avatar".to_string(),
            ProcessingProfile {
                max_input_bytes: None,
                max_output_bytes: None,
                output_width: Some(512),
                output_height: None,
            },
        )]);
        assert!(
            processing::validate_processing_profiles(
                &partial_dimensions,
                &HashMap::new(),
                DEFAULT_MAX_DECODE_DIMENSION,
            )
            .is_err()
        );

        let profiles = HashMap::from([("avatar".to_string(), ProcessingProfile::default())]);
        let unknown_assignment = HashMap::from([("users".to_string(), "missing".to_string())]);
        assert!(
            processing::validate_processing_profiles(
                &profiles,
                &unknown_assignment,
                DEFAULT_MAX_DECODE_DIMENSION,
            )
            .is_err()
        );

        let reserved_name = HashMap::from([("default".to_string(), ProcessingProfile::default())]);
        assert!(
            processing::validate_processing_profiles(
                &reserved_name,
                &HashMap::new(),
                DEFAULT_MAX_DECODE_DIMENSION,
            )
            .is_err()
        );

        let zero_input_limit = HashMap::from([(
            "tiny".to_string(),
            ProcessingProfile {
                max_input_bytes: Some(0),
                ..ProcessingProfile::default()
            },
        )]);
        assert!(
            processing::validate_processing_profiles(
                &zero_input_limit,
                &HashMap::new(),
                DEFAULT_MAX_DECODE_DIMENSION,
            )
            .is_err()
        );
    }

    #[test]
    fn profile_input_body_limit_accepts_the_largest_configured_profile_cap() {
        let global_input_bytes = 17 * 1024 * 1024;
        let profiles = HashMap::from([
            (
                "larger".to_string(),
                ProcessingProfile {
                    max_input_bytes: Some(18 * 1024 * 1024),
                    ..ProcessingProfile::default()
                },
            ),
            (
                "smaller".to_string(),
                ProcessingProfile {
                    max_input_bytes: Some(8 * 1024 * 1024),
                    ..ProcessingProfile::default()
                },
            ),
            (
                "unassigned-large".to_string(),
                ProcessingProfile {
                    max_input_bytes: Some(128 * 1024 * 1024),
                    ..ProcessingProfile::default()
                },
            ),
        ]);

        let assignments = HashMap::from([("products".to_string(), "larger".to_string())]);
        assert_eq!(
            max_configured_input_bytes(global_input_bytes, &profiles, &assignments),
            18 * 1024 * 1024
        );
        assert_eq!(
            max_configured_input_bytes(DEFAULT_MAX_SOURCE_BYTES, &profiles, &HashMap::new()),
            DEFAULT_MAX_SOURCE_BYTES
        );

        let smaller_assignment = HashMap::from([("avatars".to_string(), "smaller".to_string())]);
        assert_eq!(
            max_configured_input_bytes(global_input_bytes, &profiles, &smaller_assignment),
            global_input_bytes,
            "a lower assigned profile must not reduce the router's cap below the global limit"
        );
    }
}
