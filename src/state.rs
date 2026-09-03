use crate::{config::AppConfig, images::ImageStore};

use opendal::Operator;

use std::sync::Arc;

use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    /// Backend-independent image repository.
    pub store: ImageStore,

    pub image_jobs: Arc<Semaphore>,

    pub api_key: Option<String>,

    pub public_base_url: Option<String>,
}

impl AppState {
    /// Creates the shared application state from runtime configuration.
    pub fn from_config(config: &AppConfig) -> Result<Self, opendal::Error> {
        let operator = Operator::via_iter(&config.storage.scheme, config.storage.options.clone())?;

        Ok(Self {
            store: ImageStore::new(operator),

            image_jobs: Arc::new(Semaphore::new(config.max_concurrency)),

            api_key: config.api_key.clone(),

            public_base_url: config.public_base_url.clone(),
        })
    }
}
