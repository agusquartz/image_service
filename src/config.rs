use std::{collections::HashMap, env, io, path::PathBuf};

/// Maximum HTTP request size.
///
/// This is slightly larger than [`MAX_SOURCE_BYTES`] to leave enough
/// room for multipart headers and boundaries.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

/// Maximum accepted source image size.
///
/// Images larger than this value are rejected before processing.
pub const MAX_SOURCE_BYTES: usize = 15 * 1024 * 1024;

/// Maximum encoded output size.
///
/// Every successfully processed image must fit within this limit.
pub const MAX_OUTPUT_BYTES: usize = 400 * 1024;

/// Minimum valid image slot.
pub const MIN_SLOT: u8 = 1;

/// Maximum valid image slot.
///
/// Each resource can therefore have up to five images.
pub const MAX_SLOT: u8 = 5;

/// WebP quality levels tried before reducing image dimensions.
///
/// The encoder starts with the highest quality and progressively
/// lowers it until the result fits within [`MAX_OUTPUT_BYTES`].
pub const WEBP_QUALITIES: [f32; 7] = [88.0, 80.0, 72.0, 64.0, 56.0, 48.0, 42.0];

/// Scale applied after all WebP quality levels have been exhausted.
///
/// A value of `0.85` reduces both width and height to 85% of their
/// previous dimensions.
pub const RESIZE_SCALE: f32 = 0.85;

/// Maximum number of resize iterations.
pub const MAX_RESIZE_ROUNDS: usize = 10;

/// Stop reducing dimensions when the longest side reaches this value.
pub const MIN_LONGEST_SIDE: u32 = 512;

/// Maximum accepted width or height during image decoding.
///
/// This helps protect the service from extremely large or maliciously
/// crafted images.
pub const MAX_IMAGE_DIMENSION: u32 = 12_000;

/// Maximum memory allocation allowed by the image decoder.
pub const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

#[derive(Clone)]
pub struct StorageConfig {
    /// OpenDAL storage scheme.
    ///
    /// Examples:
    ///
    /// - fs
    /// - s3
    pub scheme: String,

    /// Backend-specific OpenDAL configuration.
    pub options: HashMap<String, String>,
}

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

    /// Optional bearer token required for write operations.
    pub api_key: Option<String>,

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
    /// - `IMAGE_API_KEY`
    /// - `IMAGE_PUBLIC_BASE_URL`
    pub fn from_env() -> io::Result<Self> {
        let bind_address = env::var("IMAGE_BIND").unwrap_or_else(|_| "127.0.0.1:3000".to_string());

        let storage_scheme = env::var("IMAGE_STORAGE_SCHEME").unwrap_or_else(|_| "fs".to_string());

        let mut storage_options = HashMap::new();

        for (key, value) in env::vars() {
            let Some(option) = key.strip_prefix("IMAGE_STORAGE_") else {
                continue;
            };

            if option == "SCHEME" || value.is_empty() {
                continue;
            }

            storage_options.insert(option.to_ascii_lowercase(), value);
        }

        if storage_scheme == "fs" && !storage_options.contains_key("root") {
            let root = dirs::data_local_dir()
                .ok_or_else(|| io::Error::other("Could not determine the local data directory"))?
                .join("image-service")
                .join("images");

            storage_options.insert("root".to_string(), root.to_string_lossy().into_owned());
        }

        if storage_scheme == "fs"
            && !storage_options.contains_key("atomic_write_dir")
            && let Some(root) = storage_options.get("root")
        {
            let atomic_write_dir = PathBuf::from(root).join(".tmp");

            storage_options.insert(
                "atomic_write_dir".to_string(),
                atomic_write_dir.to_string_lossy().into_owned(),
            );
        }

        let max_concurrency = env::var("IMAGE_MAX_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(2);

        let api_key = env::var("IMAGE_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());

        let public_base_url = env::var("IMAGE_PUBLIC_BASE_URL")
            .ok()
            .map(|value| value.trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());

        let storage = StorageConfig {
            scheme: storage_scheme,
            options: storage_options,
        };

        Ok(Self {
            bind_address,
            storage,
            max_concurrency,
            api_key,
            public_base_url,
        })
    }
}
