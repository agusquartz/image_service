pub(crate) mod dtos;
pub(crate) mod handler;

mod model;
pub(crate) mod multipart;
pub(crate) mod reduction;
pub(crate) mod service;
mod store;
pub(crate) mod validation;

// Re-export the storage abstraction because AppState needs it.
pub(crate) use model::{ImageKey, IncomingImage};
pub(crate) use store::{ImageStore, StoredMetadata};
