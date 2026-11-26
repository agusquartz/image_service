use super::{
    DEFAULT_DOWNLOAD_STREAM_CHUNK_BYTES, DEFAULT_MAX_DOWNLOADS_IN_FLIGHT,
    DEFAULT_MAX_UPLOADS_IN_FLIGHT, DEFAULT_SHUTDOWN_TIMEOUT_SECONDS,
    DEFAULT_UPLOAD_MEMORY_THRESHOLD_BYTES, DEFAULT_UPLOAD_READ_TIMEOUT_SECONDS,
    UPLOAD_BUDGET_UNIT_BYTES, io, parse_bool_env, parse_bounded_u64, parse_bounded_usize,
};

pub(super) struct TransferSettings {
    pub max_uploads_in_flight: usize,
    pub upload_memory_budget_bytes: usize,
    pub upload_memory_threshold_bytes: usize,
    pub upload_read_timeout_seconds: u64,
    pub resumable_uploads_enabled: bool,
    pub resumable_upload_ttl_seconds: u64,
    pub resumable_upload_max_chunk_bytes: usize,
    pub max_downloads_in_flight: usize,
    pub download_stream_chunk_bytes: usize,
    pub shutdown_timeout_seconds: u64,
}

pub(super) fn load(largest_input: usize) -> io::Result<TransferSettings> {
    let max_uploads_in_flight = parse_bounded_usize(
        "IMAGE_MAX_UPLOADS_IN_FLIGHT",
        DEFAULT_MAX_UPLOADS_IN_FLIGHT,
        1,
        1024,
    )?;
    let default_upload_budget = largest_input
        .div_ceil(UPLOAD_BUDGET_UNIT_BYTES)
        .checked_mul(DEFAULT_MAX_UPLOADS_IN_FLIGHT)
        .and_then(|units| units.checked_mul(UPLOAD_BUDGET_UNIT_BYTES))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "upload budget overflow"))?;
    let upload_memory_budget_bytes = parse_bounded_usize(
        "IMAGE_UPLOAD_MEMORY_BUDGET_BYTES",
        default_upload_budget,
        1,
        usize::MAX,
    )?;
    let upload_memory_threshold_bytes = parse_bounded_usize(
        "IMAGE_UPLOAD_MEMORY_THRESHOLD_BYTES",
        DEFAULT_UPLOAD_MEMORY_THRESHOLD_BYTES,
        1,
        usize::MAX,
    )?;
    let upload_units = largest_input.div_ceil(UPLOAD_BUDGET_UNIT_BYTES);
    let budget_units = upload_memory_budget_bytes / UPLOAD_BUDGET_UNIT_BYTES;
    if upload_memory_budget_bytes < largest_input
        || budget_units < upload_units
        || upload_units > u32::MAX as usize
        || budget_units > tokio::sync::Semaphore::MAX_PERMITS
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IMAGE_UPLOAD_MEMORY_BUDGET_BYTES must cover the largest configured input and fit the semaphore",
        ));
    }
    let upload_read_timeout_seconds = parse_bounded_u64(
        "IMAGE_UPLOAD_READ_TIMEOUT_SECONDS",
        DEFAULT_UPLOAD_READ_TIMEOUT_SECONDS,
        1,
        86_400,
    )?;
    let resumable_uploads_enabled = parse_bool_env("IMAGE_RESUMABLE_UPLOADS_ENABLED", false)?;
    let resumable_upload_ttl_seconds = parse_bounded_u64(
        "IMAGE_RESUMABLE_UPLOAD_TTL_SECONDS",
        86_400,
        60,
        31 * 24 * 60 * 60,
    )?;
    let resumable_upload_max_chunk_bytes = parse_bounded_usize(
        "IMAGE_RESUMABLE_UPLOAD_MAX_CHUNK_BYTES",
        DEFAULT_UPLOAD_MEMORY_THRESHOLD_BYTES,
        1,
        usize::MAX,
    )?;
    let max_downloads_in_flight = parse_bounded_usize(
        "IMAGE_MAX_DOWNLOADS_IN_FLIGHT",
        DEFAULT_MAX_DOWNLOADS_IN_FLIGHT,
        1,
        tokio::sync::Semaphore::MAX_PERMITS,
    )?;
    let download_stream_chunk_bytes = parse_bounded_usize(
        "IMAGE_DOWNLOAD_STREAM_CHUNK_BYTES",
        DEFAULT_DOWNLOAD_STREAM_CHUNK_BYTES,
        1,
        16 * 1024 * 1024,
    )?;
    let shutdown_timeout_seconds = parse_bounded_u64(
        "IMAGE_SHUTDOWN_TIMEOUT_SECONDS",
        DEFAULT_SHUTDOWN_TIMEOUT_SECONDS,
        1,
        3600,
    )?;
    Ok(TransferSettings {
        max_uploads_in_flight,
        upload_memory_budget_bytes,
        upload_memory_threshold_bytes,
        upload_read_timeout_seconds,
        resumable_uploads_enabled,
        resumable_upload_ttl_seconds,
        resumable_upload_max_chunk_bytes,
        max_downloads_in_flight,
        download_stream_chunk_bytes,
        shutdown_timeout_seconds,
    })
}
