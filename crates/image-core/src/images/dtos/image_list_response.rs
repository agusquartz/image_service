use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ImageListResponse {
    pub namespace: String,
    pub resource_id: String,
    pub max_creatable_slot: u8,
    pub images: Vec<ImageSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ImageSummary {
    pub slot: u8,
    pub url: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}
