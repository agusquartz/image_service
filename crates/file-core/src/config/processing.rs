use super::{
    DEFAULT_MAX_CONCURRENCY, DEFAULT_MAX_DECODE_ALLOCATION_BYTES, DEFAULT_MAX_DECODE_DIMENSION,
    DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_SOURCE_BYTES, DEFAULT_OUTPUT_HEIGHT,
    DEFAULT_OUTPUT_WIDTH, UPLOAD_BUDGET_UNIT_BYTES, io, parse_bounded_u32, parse_bounded_u64,
    parse_bounded_usize,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env};

const ESTIMATED_PROCESSING_BYTES_PER_OUTPUT_PIXEL: u64 = 16;
const DEFAULT_PROCESSING_MEMORY_BUDGET_BYTES: u64 = (2
    * super::DEFAULT_MAX_DECODE_ALLOCATION_BYTES
    + 1024 * 1024 * ESTIMATED_PROCESSING_BYTES_PER_OUTPUT_PIXEL)
    * super::DEFAULT_MAX_CONCURRENCY as u64;

/// Optional per-namespace processing overrides.
///
/// Omitted fields inherit the existing global processing settings.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingProfile {
    pub max_input_bytes: Option<usize>,
    pub max_output_bytes: Option<usize>,
    pub output_width: Option<u32>,
    pub output_height: Option<u32>,
}

/// Fully resolved processing policy for one upload.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProcessingPolicy {
    pub profile_name: String,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub output_width: u32,
    pub output_height: u32,
}

pub(crate) fn estimate_processing_memory_bytes(
    max_decode_allocation_bytes: u64,
    output_width: u32,
    output_height: u32,
) -> io::Result<u64> {
    let output_bytes = (output_width as u64)
        .checked_mul(output_height as u64)
        .and_then(|pixels| pixels.checked_mul(ESTIMATED_PROCESSING_BYTES_PER_OUTPUT_PIXEL))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "processing memory estimate overflow",
            )
        })?;
    max_decode_allocation_bytes
        .checked_mul(2)
        .and_then(|decode_bytes| decode_bytes.checked_add(output_bytes))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "processing memory estimate overflow",
            )
        })
}

pub(super) fn validate_processing_profiles(
    profiles: &HashMap<String, ProcessingProfile>,
    namespace_profiles: &HashMap<String, String>,
    max_decode_dimension: u32,
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
                && !(1..=max_decode_dimension).contains(&dimension)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "Profile {name} {dimension_name} must be between 1 and {max_decode_dimension}"
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

pub(super) fn parse_json_env<T>(name: &str) -> io::Result<T>
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

pub(super) struct ProcessingSettings {
    pub max_concurrency: usize,
    pub max_decode_dimension: u32,
    pub max_decode_allocation_bytes: u64,
    pub processing_memory_budget_bytes: u64,
    pub max_source_bytes: usize,
    pub max_output_bytes: usize,
    pub output_width: u32,
    pub output_height: u32,
    pub processing_profiles: HashMap<String, ProcessingProfile>,
    pub namespace_profiles: HashMap<String, String>,
    pub largest_input: usize,
}

pub(super) fn load() -> io::Result<ProcessingSettings> {
    let max_concurrency = std::env::var("IMAGE_MAX_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_CONCURRENCY);
    let max_decode_dimension = parse_bounded_u32(
        "IMAGE_MAX_DECODE_DIMENSION",
        DEFAULT_MAX_DECODE_DIMENSION,
        1,
        u32::MAX,
    )?;
    let max_decode_allocation_bytes = parse_bounded_u64(
        "IMAGE_MAX_DECODE_ALLOCATION_BYTES",
        DEFAULT_MAX_DECODE_ALLOCATION_BYTES,
        1,
        u64::MAX,
    )?;
    let processing_memory_budget_bytes = parse_bounded_u64(
        "IMAGE_PROCESSING_MEMORY_BUDGET_BYTES",
        DEFAULT_PROCESSING_MEMORY_BUDGET_BYTES,
        1,
        u64::MAX,
    )?;
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
        max_decode_dimension,
    )?;
    let output_height = parse_bounded_u32(
        "IMAGE_OUTPUT_HEIGHT",
        DEFAULT_OUTPUT_HEIGHT,
        1,
        max_decode_dimension,
    )?;

    let processing_profiles =
        parse_json_env::<HashMap<String, ProcessingProfile>>("IMAGE_PROCESSING_PROFILES")?;
    let namespace_profiles = parse_json_env::<HashMap<String, String>>("IMAGE_NAMESPACE_PROFILES")?;
    validate_processing_profiles(
        &processing_profiles,
        &namespace_profiles,
        max_decode_dimension,
    )?;
    let max_profile_width = processing_profiles
        .values()
        .filter_map(|profile| profile.output_width)
        .max()
        .unwrap_or(output_width)
        .max(output_width);
    let max_profile_height = processing_profiles
        .values()
        .filter_map(|profile| profile.output_height)
        .max()
        .unwrap_or(output_height)
        .max(output_height);
    let minimum_processing_budget = estimate_processing_memory_bytes(
        max_decode_allocation_bytes,
        max_profile_width,
        max_profile_height,
    )?;
    let processing_budget_units = processing_memory_budget_bytes / UPLOAD_BUDGET_UNIT_BYTES as u64;
    let minimum_processing_units =
        minimum_processing_budget.div_ceil(UPLOAD_BUDGET_UNIT_BYTES as u64);
    if processing_budget_units < minimum_processing_units
        || processing_budget_units > tokio::sync::Semaphore::MAX_PERMITS as u64
        || max_decode_allocation_bytes > usize::MAX as u64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IMAGE_PROCESSING_MEMORY_BUDGET_BYTES must fit at least one configured processing operation and the Tokio semaphore; increase it or lower decode/output limits",
        ));
    }

    let largest_input =
        max_configured_input_bytes(max_source_bytes, &processing_profiles, &namespace_profiles);
    Ok(ProcessingSettings {
        max_concurrency,
        max_decode_dimension,
        max_decode_allocation_bytes,
        processing_memory_budget_bytes,
        max_source_bytes,
        max_output_bytes,
        output_width,
        output_height,
        processing_profiles,
        namespace_profiles,
        largest_input,
    })
}
