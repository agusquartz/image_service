use crate::{
    config::{
        AppConfig, AuthRequirement, ProcessingPolicy, ProcessingProfile,
        max_configured_input_bytes, resolve_processing_policy,
    },
    images::ImageStore,
    observability::HttpMetrics,
    rate_limit::RateLimiter,
};

use opendal::Operator;

use std::{collections::HashMap, sync::Arc};

use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    /// Backend-independent image repository.
    pub store: ImageStore,

    pub image_jobs: Arc<Semaphore>,

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
    pub fn from_config(config: &AppConfig) -> Result<Self, opendal::Error> {
        let operator = Operator::via_iter(&config.storage.scheme, config.storage.options.clone())?;

        Ok(Self {
            store: ImageStore::new(operator),

            image_jobs: Arc::new(Semaphore::new(config.max_concurrency)),

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
}
