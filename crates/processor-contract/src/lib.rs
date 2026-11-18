use bytes::Bytes;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Capability {
    Image,
    Video,
    Document,
    Blob,
}

#[derive(Clone, Debug)]
pub struct ProcessingRequest {
    pub source: Bytes,
    pub declared_media_type: Option<String>,
    pub max_output_bytes: usize,
    pub output_width: u32,
    pub output_height: u32,
    pub queue_wait_us: u64,
}

#[derive(Clone, Debug)]
pub struct ProcessingOutput {
    pub bytes: Bytes,
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessingError {
    UnsupportedMediaType(String),
    InvalidInput(String),
    Failed(String),
}

impl fmt::Display for ProcessingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMediaType(message)
            | Self::InvalidInput(message)
            | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ProcessingError {}

pub trait Processor: Send + Sync {
    fn capability(&self) -> Capability;

    fn process(&self, request: ProcessingRequest) -> Result<ProcessingOutput, ProcessingError>;
}
