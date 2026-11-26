use super::{env, io, parse_bool_env, parse_bounded_u64, parse_bounded_usize};
use std::path::PathBuf;

pub(super) struct JobSettings {
    pub database_path: PathBuf,
    pub background_concurrency: usize,
    pub background_idle_seconds: u64,
    pub background_window_utc: Option<String>,
    pub retention_days: u64,
    pub async_image_max_jobs: usize,
    pub async_image_max_bytes: u64,
    pub storage_usage_enabled: bool,
    pub storage_catalog_path: PathBuf,
    pub storage_inventory_interval_seconds: u64,
}

pub(super) fn load() -> io::Result<JobSettings> {
    let database_path = configured_path("IMAGE_JOB_DB_PATH", "jobs.sqlite3", None)?;
    let background_concurrency = parse_bounded_usize("IMAGE_BACKGROUND_JOB_CONCURRENCY", 1, 1, 64)?;
    let background_idle_seconds =
        parse_bounded_u64("IMAGE_BACKGROUND_IDLE_SECONDS", 30, 0, 86_400)?;
    let retention_days = parse_bounded_u64("IMAGE_JOB_RETENTION_DAYS", 30, 1, 3650)?;
    let async_image_max_jobs = parse_bounded_usize("IMAGE_ASYNC_MAX_JOBS", 1000, 1, 1000)?;
    let async_image_max_bytes =
        parse_bounded_u64("IMAGE_ASYNC_MAX_STAGED_BYTES", 1_073_741_824, 1, u64::MAX)?;
    let storage_usage_enabled = parse_bool_env("IMAGE_STORAGE_USAGE_ENABLED", false)?;
    let storage_catalog_path = configured_path(
        "IMAGE_STORAGE_CATALOG_DB_PATH",
        "storage-catalog.sqlite3",
        Some(&database_path),
    )?;
    let storage_inventory_interval_seconds = parse_bounded_u64(
        "IMAGE_STORAGE_INVENTORY_INTERVAL_SECONDS",
        3600,
        60,
        31 * 24 * 60 * 60,
    )?;
    let background_window_utc = env::var("IMAGE_BACKGROUND_WINDOW_UTC")
        .ok()
        .map(|window| window.trim().to_string())
        .filter(|window| !window.is_empty());
    if let Some(window) = &background_window_utc {
        job_scheduler::DailyUtcWindow::parse(window)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    }

    Ok(JobSettings {
        database_path,
        background_concurrency,
        background_idle_seconds,
        background_window_utc,
        retention_days,
        async_image_max_jobs,
        async_image_max_bytes,
        storage_usage_enabled,
        storage_catalog_path,
        storage_inventory_interval_seconds,
    })
}

fn configured_path(
    name: &str,
    default_file: &str,
    sibling_of: Option<&PathBuf>,
) -> io::Result<PathBuf> {
    match env::var(name) {
        Ok(path) if !path.trim().is_empty() => Ok(PathBuf::from(path)),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} cannot be empty"),
        )),
        Err(env::VarError::NotPresent) => {
            if let Some(sibling) = sibling_of {
                Ok(sibling.with_file_name(default_file))
            } else {
                Ok(dirs::data_local_dir()
                    .ok_or_else(|| {
                        io::Error::other("Could not determine the local data directory")
                    })?
                    .join("image-service")
                    .join(default_file))
            }
        }
        Err(error) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Could not read {name}: {error}"),
        )),
    }
}
