use super::{DEFAULT_IMAGE_CACHE_TTL_SECONDS, env, io, security::AuthRequirement};

pub(super) struct CacheSettings {
    pub cache_control: String,
    pub public_base_url: Option<String>,
}

pub(super) fn load(read_auth: AuthRequirement) -> io::Result<CacheSettings> {
    let configured_cache_control = env::var("IMAGE_CACHE_CONTROL")
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
    let cache_control = configured_cache_control.unwrap_or_else(|| {
        if cache_ttl_seconds == 0 {
            "no-store".to_string()
        } else {
            let visibility = if read_auth.is_required() {
                "private"
            } else {
                "public"
            };
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

    Ok(CacheSettings {
        cache_control,
        public_base_url,
    })
}
