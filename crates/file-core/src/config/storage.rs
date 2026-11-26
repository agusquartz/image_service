use super::{env, io};
use std::{collections::HashMap, path::PathBuf};

#[derive(Clone)]
pub struct StorageConfig {
    /// OpenDAL storage scheme, such as `fs` or `s3`.
    pub scheme: String,
    /// Backend-specific OpenDAL configuration.
    pub options: HashMap<String, String>,
}

pub(super) fn load() -> io::Result<StorageConfig> {
    let scheme = env::var("IMAGE_STORAGE_SCHEME").unwrap_or_else(|_| "fs".to_string());
    let mut options = HashMap::new();

    for (key, value) in env::vars() {
        let Some(option) = key.strip_prefix("IMAGE_STORAGE_") else {
            continue;
        };
        if option == "SCHEME" || value.is_empty() {
            continue;
        }
        options.insert(option.to_ascii_lowercase(), value);
    }

    if scheme == "fs" && !options.contains_key("root") {
        let root = dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("Could not determine the local data directory"))?
            .join("image-service")
            .join("images");
        options.insert("root".to_string(), root.to_string_lossy().into_owned());
    }

    if scheme == "fs"
        && !options.contains_key("atomic_write_dir")
        && let Some(root) = options.get("root")
    {
        let atomic_write_dir = PathBuf::from(root).join(".tmp");
        options.insert(
            "atomic_write_dir".to_string(),
            atomic_write_dir.to_string_lossy().into_owned(),
        );
    }

    Ok(StorageConfig { scheme, options })
}
