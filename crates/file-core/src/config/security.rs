use super::{
    DEFAULT_MAX_SLOTS, MAX_PRESIGN_TTL_SECONDS, MAX_SLOT_NUMBER, env, io, parse_bounded_u32,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    Required,
    None,
}

impl AuthRequirement {
    pub(super) fn parse(name: &str, default: Self) -> io::Result<Self> {
        match env::var(name) {
            Ok(value) => Self::parse_value(name, &value),
            Err(env::VarError::NotPresent) => Ok(default),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Could not read {name}: {error}"),
            )),
        }
    }

    pub(super) fn parse_value(name: &str, value: &str) -> io::Result<Self> {
        if value.eq_ignore_ascii_case("required") {
            Ok(Self::Required)
        } else if value.eq_ignore_ascii_case("none") {
            Ok(Self::None)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be either 'required' or 'none'"),
            ))
        }
    }

    pub fn is_required(self) -> bool {
        self == Self::Required
    }
}

pub(super) fn validate_storage_usage_auth(enabled: bool, api_key: Option<&str>) -> io::Result<()> {
    if enabled && api_key.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IMAGE_API_KEY is required when IMAGE_STORAGE_USAGE_ENABLED=true",
        ));
    }
    Ok(())
}

pub(super) struct SecuritySettings {
    pub max_slots: u8,
    pub api_key: Option<String>,
    pub read_auth: AuthRequirement,
    pub write_auth: AuthRequirement,
    pub presign_ttl_seconds: u64,
}

pub(super) fn load(storage_usage_enabled: bool) -> io::Result<SecuritySettings> {
    let max_slots = parse_bounded_u32(
        "IMAGE_MAX_SLOTS",
        u32::from(DEFAULT_MAX_SLOTS),
        1,
        u32::from(MAX_SLOT_NUMBER),
    )? as u8;
    let api_key = env::var("IMAGE_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty());

    if env::var_os("GETMETHOD_AUTH").is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GETMETHOD_AUTH has been removed; use IMAGE_READ_AUTH and IMAGE_WRITE_AUTH",
        ));
    }
    let read_auth = AuthRequirement::parse("IMAGE_READ_AUTH", AuthRequirement::Required)?;
    let write_auth = AuthRequirement::parse("IMAGE_WRITE_AUTH", AuthRequirement::Required)?;
    if (read_auth.is_required() || write_auth.is_required()) && api_key.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IMAGE_API_KEY is required when IMAGE_READ_AUTH or IMAGE_WRITE_AUTH is 'required'",
        ));
    }
    validate_storage_usage_auth(storage_usage_enabled, api_key.as_deref())?;

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
            format!("IMAGE_PRESIGN_TTL_SECONDS must be between 1 and {MAX_PRESIGN_TTL_SECONDS}"),
        ));
    }

    Ok(SecuritySettings {
        max_slots,
        api_key,
        read_auth,
        write_auth,
        presign_ttl_seconds,
    })
}
