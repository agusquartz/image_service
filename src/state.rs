use crate::{config::AppConfig, images::ImageStore};

use opendal::Operator;

use std::sync::Arc;

use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    /// Backend-independent image repository.
    pub store: ImageStore,

    pub image_jobs: Arc<Semaphore>,

    pub max_source_bytes: usize,

    pub max_output_bytes: usize,

    pub output_width: u32,

    pub output_height: u32,

    pub max_slots: u8,

    pub api_key: Option<String>,

    pub getmethod_auth: bool,

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

            max_output_bytes: config.max_output_bytes,

            output_width: config.output_width,

            output_height: config.output_height,

            max_slots: config.max_slots,

            api_key: config.api_key.clone(),

            getmethod_auth: config.getmethod_auth,

            presign_ttl_seconds: config.presign_ttl_seconds,

            cache_control: config.cache_control.clone(),

            public_base_url: config.public_base_url.clone(),
        })
    }
}
