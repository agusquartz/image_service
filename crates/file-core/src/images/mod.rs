pub(crate) mod dtos;
pub(crate) mod handler;

mod model;
mod multipart;
mod reduction;
mod service;
mod store;
pub(crate) mod validation;

// Re-export the storage abstraction because AppState needs it.
pub(crate) use store::ImageStore;
