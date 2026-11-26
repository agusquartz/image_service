use super::{io, parse_bool_env, parse_bounded_u32, parse_bounded_u64};

pub const DEFAULT_READ_RATE_LIMIT_REQUESTS: u32 = 600;
pub const DEFAULT_WRITE_RATE_LIMIT_REQUESTS: u32 = 60;
pub const DEFAULT_RATE_LIMIT_WINDOW_SECONDS: u64 = 60;

#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub read_requests: u32,
    pub write_requests: u32,
    pub window_seconds: u64,
}

pub(super) fn validate_metrics_auth(
    metrics_enabled: bool,
    metrics_auth: bool,
    api_key: Option<&str>,
) -> io::Result<()> {
    if metrics_enabled && metrics_auth && api_key.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IMAGE_METRICS_ENABLED=true with IMAGE_METRICS_AUTH=true requires IMAGE_API_KEY to be configured",
        ));
    }
    Ok(())
}

pub(super) struct ObservabilitySettings {
    pub rate_limit: RateLimitConfig,
    pub metrics_enabled: bool,
    pub metrics_auth: bool,
}

pub(super) fn load(api_key: Option<&str>) -> io::Result<ObservabilitySettings> {
    let rate_limit = RateLimitConfig {
        read_requests: parse_bounded_u32(
            "IMAGE_RATE_LIMIT_READ_REQUESTS",
            DEFAULT_READ_RATE_LIMIT_REQUESTS,
            0,
            u32::MAX,
        )?,
        write_requests: parse_bounded_u32(
            "IMAGE_RATE_LIMIT_WRITE_REQUESTS",
            DEFAULT_WRITE_RATE_LIMIT_REQUESTS,
            0,
            u32::MAX,
        )?,
        window_seconds: parse_bounded_u64(
            "IMAGE_RATE_LIMIT_WINDOW_SECONDS",
            DEFAULT_RATE_LIMIT_WINDOW_SECONDS,
            1,
            86_400,
        )?,
    };
    let metrics_enabled = parse_bool_env("IMAGE_METRICS_ENABLED", false)?;
    let metrics_auth = parse_bool_env("IMAGE_METRICS_AUTH", true)?;
    validate_metrics_auth(metrics_enabled, metrics_auth, api_key)?;
    Ok(ObservabilitySettings {
        rate_limit,
        metrics_enabled,
        metrics_auth,
    })
}
