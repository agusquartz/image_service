use crate::{
    config::AppConfig,
    images::ImageStore,
};

use std::sync::Arc;

use tokio::sync::Semaphore;

/// Dependencies shared between HTTP handlers.
///
/// `AppState` should contain application-level dependencies, not
/// request-specific data.
#[derive(Clone)]
pub struct AppState {
    /// Filesystem-backed image repository.
    pub store: ImageStore,

    /// Limits how many CPU-intensive image operations may execute
    /// simultaneously.
    pub image_jobs: Arc<Semaphore>,

    /// Optional API key used to protect write operations.
    pub api_key: Option<String>,

    /// Optional public origin used when generating image URLs.
    pub public_base_url: Option<String>,
}

impl AppState {
    /// Creates the shared application state from runtime configuration.
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            store: ImageStore::new(
                config.image_root.clone()
            ),

            image_jobs: Arc::new(
                Semaphore::new(
                    config.max_concurrency
                )
            ),

            api_key: config.api_key.clone(),

            public_base_url:
                config.public_base_url.clone(),
        }
    }
}
