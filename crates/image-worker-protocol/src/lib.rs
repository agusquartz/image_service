use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerCapability {
    Image,
    Video,
    Document,
    Blob,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageRef {
    pub storage_id: String,
    pub key: String,
    pub version: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerRegistration {
    pub protocol_version: u16,
    pub worker_id: String,
    pub capabilities: Vec<WorkerCapability>,
    pub max_in_flight: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct JobAssignment {
    pub protocol_version: u16,
    pub job_id: String,
    pub generation: u64,
    pub capability: WorkerCapability,
    pub input: StorageRef,
    pub options: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobOutcome {
    Succeeded { output: StorageRef },
    Failed { error_code: String, retryable: bool },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobResult {
    pub protocol_version: u16,
    pub job_id: String,
    pub generation: u64,
    pub outcome: JobOutcome,
}

#[cfg(test)]
mod tests {
    use super::{
        JobAssignment, JobOutcome, JobResult, PROTOCOL_VERSION, StorageRef, WorkerCapability,
        WorkerRegistration,
    };

    #[test]
    fn registration_and_storage_reference_round_trip_as_versioned_json() {
        let registration = WorkerRegistration {
            protocol_version: PROTOCOL_VERSION,
            worker_id: "image-worker-1".to_string(),
            capabilities: vec![WorkerCapability::Image],
            max_in_flight: 4,
        };
        let encoded = serde_json::to_vec(&registration).unwrap();
        let decoded: WorkerRegistration = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, registration);

        let storage_ref = StorageRef {
            storage_id: "primary".to_string(),
            key: "staging/job-1/input".to_string(),
            version: Some("etag-1".to_string()),
            size_bytes: Some(123),
            sha256: Some("abc123".to_string()),
        };
        let encoded = serde_json::to_vec(&storage_ref).unwrap();
        let decoded: StorageRef = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, storage_ref);
    }

    #[test]
    fn job_assignment_and_tagged_result_round_trip() {
        let assignment = JobAssignment {
            protocol_version: PROTOCOL_VERSION,
            job_id: "job-1".to_string(),
            generation: 7,
            capability: WorkerCapability::Image,
            input: StorageRef {
                storage_id: "primary".to_string(),
                key: "staging/job-1/input".to_string(),
                version: None,
                size_bytes: Some(123),
                sha256: None,
            },
            options: serde_json::json!({"format": "webp"}),
        };
        let encoded = serde_json::to_vec(&assignment).unwrap();
        let decoded: JobAssignment = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.job_id, assignment.job_id);
        assert_eq!(decoded.generation, assignment.generation);
        assert_eq!(decoded.options, assignment.options);

        let result = JobResult {
            protocol_version: PROTOCOL_VERSION,
            job_id: assignment.job_id,
            generation: assignment.generation,
            outcome: JobOutcome::Failed {
                error_code: "invalid_input".to_string(),
                retryable: false,
            },
        };
        let encoded = serde_json::to_vec(&result).unwrap();
        assert!(String::from_utf8_lossy(&encoded).contains("\"status\":\"failed\""));
        let decoded: JobResult = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, result);
    }
}
