use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct PresignResponse {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub expires_in: u64,
}
