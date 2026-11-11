use serde::Deserialize;
use std::{collections::HashMap, env, io, path::PathBuf};

/// Default maximum accepted source image size.
///
/// Images larger than this value are rejected before processing.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 15 * 1024 * 1024;

/// Default maximum encoded output size.
///
/// Every successfully processed image must fit within this limit.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 100 * 1024;

/// Default freshness lifetime for image responses, in seconds.
pub const DEFAULT_IMAGE_CACHE_TTL_SECONDS: u64 = 60;

/// Maximum HTTP request overhead for multipart headers and boundaries.
pub const MULTIPART_OVERHEAD_BYTES: usize = 1024 * 1024;

/// Minimum valid image slot.
pub const MIN_SLOT: u8 = 1;

/// Default maximum number of slots available to each resource.
pub const DEFAULT_MAX_SLOTS: u8 = 5;

/// Slot path parameters and storage keys use an unsigned 8-bit slot number.
pub const MAX_SLOT_NUMBER: u8 = u8::MAX;

/// Maximum accepted width or height during image decoding.
///
/// This helps protect the service from extremely large or maliciously
/// crafted images.
pub const MAX_IMAGE_DIMENSION: u32 = 12_000;

/// Maximum source image dimensions accepted for converted output.
pub const DEFAULT_OUTPUT_WIDTH: u32 = 1024;
pub const DEFAULT_OUTPUT_HEIGHT: u32 = 1024;

/// Maximum memory allocation allowed by the image decoder.
pub const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Maximum presigned URL lifetime supported by the S3 API.
pub const MAX_PRESIGN_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

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

/// Optional per-namespace processing overrides.
///
/// Omitted fields inherit the existing global processing settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingProfile {
    pub max_input_bytes: Option<usize>,
    pub max_output_bytes: Option<usize>,
    pub output_width: Option<u32>,
    pub output_height: Option<u32>,
}

/// Fully resolved processing policy for one upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingPolicy {
    pub profile_name: String,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub output_width: u32,
    pub output_height: u32,
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

    /// Optional bearer token required for write operations.
    pub api_key: Option<String>,

    /// Whether image reads require the configured bearer token.
    ///
    /// Defaults to `false` to preserve the existing public GET behavior.
    pub getmethod_auth: bool,

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
    /// - `IMAGE_MAX_INPUT_BYTES`
    /// - `IMAGE_MAX_OUTPUT_BYTES`
    /// - `IMAGE_OUTPUT_WIDTH`
    /// - `IMAGE_OUTPUT_HEIGHT`
    /// - `IMAGE_PROCESSING_PROFILES` (JSON object)
    /// - `IMAGE_NAMESPACE_PROFILES` (JSON object)
    /// - `IMAGE_MAX_SLOTS`
    /// - `IMAGE_API_KEY`
    /// - `GETMETHOD_AUTH`
    /// - `IMAGE_PRESIGN_TTL_SECONDS`
    /// - `IMAGE_CACHE_CONTROL`
    /// - `IMAGE_CACHE_TTL_SECONDS`
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

        let max_source_bytes = parse_bounded_usize(
            "IMAGE_MAX_INPUT_BYTES",
            DEFAULT_MAX_SOURCE_BYTES,
            1,
            usize::MAX,
        )?;
        let max_output_bytes = parse_bounded_usize(
            "IMAGE_MAX_OUTPUT_BYTES",
            DEFAULT_MAX_OUTPUT_BYTES,
            1,
            usize::MAX,
        )?;
        let output_width = parse_bounded_u32(
            "IMAGE_OUTPUT_WIDTH",
            DEFAULT_OUTPUT_WIDTH,
            1,
            MAX_IMAGE_DIMENSION,
        )?;
        let output_height = parse_bounded_u32(
            "IMAGE_OUTPUT_HEIGHT",
            DEFAULT_OUTPUT_HEIGHT,
            1,
            MAX_IMAGE_DIMENSION,
        )?;

        let processing_profiles =
            parse_json_env::<HashMap<String, ProcessingProfile>>("IMAGE_PROCESSING_PROFILES")?;
        let namespace_profiles =
            parse_json_env::<HashMap<String, String>>("IMAGE_NAMESPACE_PROFILES")?;
        validate_processing_profiles(&processing_profiles, &namespace_profiles)?;
        let max_slots = parse_bounded_u32(
            "IMAGE_MAX_SLOTS",
            u32::from(DEFAULT_MAX_SLOTS),
            1,
            u32::from(MAX_SLOT_NUMBER),
        )? as u8;

        let api_key = env::var("IMAGE_API_KEY")
            .ok()
            .filter(|value| !value.is_empty());

        let getmethod_auth = env::var("GETMETHOD_AUTH")
            .ok()
            .and_then(|value| value.parse::<bool>().ok())
            .unwrap_or(false);

        if getmethod_auth && api_key.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "GETMETHOD_AUTH=true requires IMAGE_API_KEY to be configured",
            ));
        }

        let presign_ttl_seconds = env::var("IMAGE_PRESIGN_TTL_SECONDS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("IMAGE_PRESIGN_TTL_SECONDS must be a positive integer: {error}"),
                )
            })?
            .unwrap_or(3600);

        if presign_ttl_seconds == 0 || presign_ttl_seconds > MAX_PRESIGN_TTL_SECONDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "IMAGE_PRESIGN_TTL_SECONDS must be between 1 and {MAX_PRESIGN_TTL_SECONDS}"
                ),
            ));
        }

        let cache_control = env::var("IMAGE_CACHE_CONTROL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let cache_ttl_seconds = env::var("IMAGE_CACHE_TTL_SECONDS")
            .ok()
            .map(|value| value.parse::<u64>())
            .transpose()
            .map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("IMAGE_CACHE_TTL_SECONDS must be a non-negative integer: {error}"),
                )
            })?
            .unwrap_or(DEFAULT_IMAGE_CACHE_TTL_SECONDS);

        let cache_control = cache_control.unwrap_or_else(|| {
            if cache_ttl_seconds == 0 {
                "no-store".to_string()
            } else {
                let visibility = if getmethod_auth { "private" } else { "public" };
                format!("{visibility}, max-age={cache_ttl_seconds}")
            }
        });

        if axum::http::HeaderValue::from_str(&cache_control).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "IMAGE_CACHE_CONTROL must be a valid HTTP header value",
            ));
        }

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
            max_source_bytes,
            max_output_bytes,
            output_width,
            output_height,
            processing_profiles,
            namespace_profiles,
            max_slots,
            api_key,
            getmethod_auth,
            presign_ttl_seconds,
            cache_control,
            public_base_url,
        })
    }
}

fn parse_json_env<T>(name: &str) -> io::Result<T>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let Ok(value) = env::var(name) else {
        return Ok(T::default());
    };
    if value.trim().is_empty() {
        return Ok(T::default());
    }
    serde_json::from_str(&value).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must contain a valid JSON object: {error}"),
        )
    })
}

fn validate_processing_profiles(
    profiles: &HashMap<String, ProcessingProfile>,
    namespace_profiles: &HashMap<String, String>,
) -> io::Result<()> {
    for (name, profile) in profiles {
        if name == "default" || !is_valid_profile_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid or reserved processing profile name: {name}"),
            ));
        }
        if profile.max_input_bytes == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Profile {name} max_input_bytes must be greater than zero"),
            ));
        }
        if profile.max_output_bytes == Some(0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Profile {name} max_output_bytes must be greater than zero"),
            ));
        }
        if profile.output_width.is_some() != profile.output_height.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Profile {name} must set output_width and output_height together"),
            ));
        }
        for (dimension_name, dimension) in [
            ("output_width", profile.output_width),
            ("output_height", profile.output_height),
        ] {
            if let Some(dimension) = dimension
                && !(1..=MAX_IMAGE_DIMENSION).contains(&dimension)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Profile {name} {dimension_name} must be between 1 and {MAX_IMAGE_DIMENSION}"
                    ),
                ));
            }
        }
    }

    for (namespace, profile_name) in namespace_profiles {
        if namespace.is_empty()
            || namespace.len() > 32
            || !namespace.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || character == '-'
                    || character == '_'
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid namespace in IMAGE_NAMESPACE_PROFILES: {namespace}"),
            ));
        }
        if !profiles.contains_key(profile_name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Namespace {namespace} references undefined image processing profile: {profile_name}"
                ),
            ));
        }
    }

    Ok(())
}

fn is_valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
}

pub(crate) fn resolve_processing_policy(
    namespace: &str,
    global_max_input_bytes: usize,
    global_max_output_bytes: usize,
    global_output_width: u32,
    global_output_height: u32,
    profiles: &HashMap<String, ProcessingProfile>,
    namespace_profiles: &HashMap<String, String>,
) -> ProcessingPolicy {
    let Some(profile_name) = namespace_profiles.get(namespace) else {
        return ProcessingPolicy {
            profile_name: "default".to_string(),
            max_input_bytes: global_max_input_bytes,
            max_output_bytes: global_max_output_bytes,
            output_width: global_output_width,
            output_height: global_output_height,
        };
    };

    let profile = &profiles[profile_name];
    ProcessingPolicy {
        profile_name: profile_name.clone(),
        max_input_bytes: profile.max_input_bytes.unwrap_or(global_max_input_bytes),
        max_output_bytes: profile.max_output_bytes.unwrap_or(global_max_output_bytes),
        output_width: profile.output_width.unwrap_or(global_output_width),
        output_height: profile.output_height.unwrap_or(global_output_height),
    }
}

pub(crate) fn max_configured_input_bytes(
    global_max_input_bytes: usize,
    profiles: &HashMap<String, ProcessingProfile>,
    namespace_profiles: &HashMap<String, String>,
) -> usize {
    namespace_profiles
        .values()
        .filter_map(|profile_name| profiles.get(profile_name))
        .filter_map(|profile| profile.max_input_bytes)
        .fold(global_max_input_bytes, usize::max)
}

fn parse_bounded_usize(name: &str, default: usize, min: usize, max: usize) -> io::Result<usize> {
    let value = env::var(name)
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be a positive integer: {error}"),
            )
        })?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {min} and {max}"),
        ));
    }
    Ok(value)
}

fn parse_bounded_u32(name: &str, default: u32, min: u32, max: u32) -> io::Result<u32> {
    let value = env::var(name)
        .ok()
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be a positive integer: {error}"),
            )
        })?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {min} and {max}"),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(validate_processing_profiles(&partial_dimensions, &HashMap::new()).is_err());

        let profiles = HashMap::from([("avatar".to_string(), ProcessingProfile::default())]);
        let unknown_assignment = HashMap::from([("users".to_string(), "missing".to_string())]);
        assert!(validate_processing_profiles(&profiles, &unknown_assignment).is_err());

        let reserved_name = HashMap::from([("default".to_string(), ProcessingProfile::default())]);
        assert!(validate_processing_profiles(&reserved_name, &HashMap::new()).is_err());

        let zero_input_limit = HashMap::from([(
            "tiny".to_string(),
            ProcessingProfile {
                max_input_bytes: Some(0),
                ..ProcessingProfile::default()
            },
        )]);
        assert!(validate_processing_profiles(&zero_input_limit, &HashMap::new()).is_err());
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
