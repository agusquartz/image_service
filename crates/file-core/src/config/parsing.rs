use std::{env, io};

pub(super) fn parse_bounded_usize(
    name: &str,
    default: usize,
    min: usize,
    max: usize,
) -> io::Result<usize> {
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
    validate_bounded_usize(name, value, min, max)
}

pub(super) fn validate_bounded_usize(
    name: &str,
    value: usize,
    min: usize,
    max: usize,
) -> io::Result<usize> {
    if !(min..=max).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {min} and {max}"),
        ));
    }
    Ok(value)
}

pub(super) fn parse_bounded_u32(name: &str, default: u32, min: u32, max: u32) -> io::Result<u32> {
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

pub(super) fn parse_bounded_u64(name: &str, default: u64, min: u64, max: u64) -> io::Result<u64> {
    let value = env::var(name)
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be a positive integer: {error}"),
            )
        })?
        .unwrap_or(default);
    validate_bounded_u64(name, value, min, max)
}

pub(super) fn validate_bounded_u64(name: &str, value: u64, min: u64, max: u64) -> io::Result<u64> {
    if !(min..=max).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {min} and {max}"),
        ));
    }
    Ok(value)
}

pub(super) fn parse_bool_env(name: &str, default: bool) -> io::Result<bool> {
    match env::var(name) {
        Ok(value) => parse_bool_value(name, Some(&value), default),
        Err(env::VarError::NotPresent) => parse_bool_value(name, None, default),
        Err(error) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Could not read {name}: {error}"),
        )),
    }
}

pub(super) fn parse_bool_value(name: &str, value: Option<&str>, default: bool) -> io::Result<bool> {
    match value {
        Some(value) => value.parse::<bool>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be 'true' or 'false': {error}"),
            )
        }),
        None => Ok(default),
    }
}
