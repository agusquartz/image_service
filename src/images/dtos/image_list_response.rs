use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ImageListResponse {
    pub namespace: String,
    pub resource_id: String,
    pub images: Vec<ImageSummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ImageSummary {
    pub slot: u8,
    pub url: String,
}
