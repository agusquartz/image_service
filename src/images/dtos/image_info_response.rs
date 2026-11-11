use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ImageInfoResponse {
    pub namespace: String,
    pub resource_id: String,
    pub slot: u8,
    pub size_bytes: u64,
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
    pub profile: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}
