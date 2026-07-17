use git_cdc_core::ObjectManifest;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Request to create or resume a chunk-aware upload session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BeginUploadRequest {
    /// Git-CDC transfer protocol version.
    pub protocol_version: u16,
    /// Complete ordered manifest proposed for the logical object.
    pub manifest: ObjectManifest,
}

/// Server plan describing which chunks a client must upload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BeginUploadResponse {
    /// Git-CDC transfer protocol version selected by the server.
    pub protocol_version: u16,
    /// Stable, resumable upload session identifier.
    pub upload_id: Uuid,
    /// Zero-based indexes into the submitted manifest that are not stored.
    pub missing_chunk_indexes: Vec<u32>,
    /// RFC 3339 expiry timestamp for the upload session.
    pub expires_at: String,
}
