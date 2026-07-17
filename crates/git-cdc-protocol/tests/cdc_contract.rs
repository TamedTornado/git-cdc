//! Contract tests for the versioned chunk-aware protocol.
#![allow(
    clippy::unwrap_used,
    reason = "literal wire fixtures should fail immediately when malformed"
)]

use std::io::Cursor;

use git_cdc_core::{ChunkStream, ChunkingProfile};
use git_cdc_protocol::{BeginUploadRequest, BeginUploadResponse};
use uuid::Uuid;

#[test]
fn upload_plan_round_trips_without_losing_manifest_order() {
    let manifest = ChunkStream::new(
        Cursor::new(vec![0x5A; 9 * 1024 * 1024]),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();
    let request = BeginUploadRequest {
        protocol_version: 1,
        manifest: manifest.clone(),
    };

    let json = serde_json::to_string(&request).unwrap();
    let decoded: BeginUploadRequest = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded, request);
    assert_eq!(decoded.manifest.chunks, manifest.chunks);
}

#[test]
fn missing_chunks_are_indexes_into_the_submitted_manifest() {
    let response = BeginUploadResponse {
        protocol_version: 1,
        upload_id: Uuid::nil(),
        missing_chunk_indexes: vec![0, 3, 9],
        expires_at: "2026-07-18T00:00:00Z".into(),
    };

    assert_eq!(
        serde_json::to_value(response).unwrap()["missing_chunk_indexes"],
        serde_json::json!([0, 3, 9])
    );
}
