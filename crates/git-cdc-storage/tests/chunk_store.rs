//! Behavioral contract shared by object-store providers.
#![allow(
    clippy::unwrap_used,
    reason = "storage contract setup should fail immediately"
)]

use std::sync::Arc;

use git_cdc_core::ChunkId;
use git_cdc_storage::{ChunkStore, StorageError};
use object_store::{ObjectStore, memory::InMemory};
use uuid::Uuid;

fn id(bytes: &[u8]) -> ChunkId {
    blake3::hash(bytes).to_hex().as_str().parse().unwrap()
}

async fn contract(store: Arc<dyn ObjectStore>) {
    let chunks = ChunkStore::new(store);
    let repository_id = Uuid::nil();
    let content = bytes::Bytes::from_static(b"immutable chunk bytes");
    let chunk_id = id(&content);

    assert!(!chunks.exists(repository_id, chunk_id).await.unwrap());
    chunks
        .put_verified(repository_id, chunk_id, content.clone())
        .await
        .unwrap();
    chunks
        .put_verified(repository_id, chunk_id, content.clone())
        .await
        .unwrap();
    assert!(chunks.exists(repository_id, chunk_id).await.unwrap());
    assert_eq!(
        chunks.get_verified(repository_id, chunk_id).await.unwrap(),
        content
    );
    chunks.delete(repository_id, chunk_id).await.unwrap();
    assert!(!chunks.exists(repository_id, chunk_id).await.unwrap());
}

#[tokio::test]
async fn in_memory_provider_obeys_the_chunk_contract() {
    contract(Arc::new(InMemory::new())).await;
}

#[tokio::test]
async fn filesystem_provider_obeys_the_chunk_contract() {
    let directory = tempfile::tempdir().unwrap();
    let store = object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap();
    contract(Arc::new(store)).await;
}

#[tokio::test]
async fn minio_provider_obeys_the_chunk_contract_when_configured() {
    if std::env::var_os("GIT_CDC_TEST_MINIO").is_none() {
        return;
    }
    let store = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name("git-cdc-test")
        .with_region("us-east-1")
        .with_endpoint("http://127.0.0.1:59000")
        .with_access_key_id("git_cdc")
        .with_secret_access_key("git_cdc_secret")
        .with_allow_http(true)
        .build()
        .unwrap();
    contract(Arc::new(store)).await;
}

#[tokio::test]
async fn refuses_bytes_that_do_not_match_the_claimed_chunk_id() {
    let chunks = ChunkStore::new(Arc::new(InMemory::new()));
    let claimed = id(b"different");

    let error = chunks
        .put_verified(Uuid::nil(), claimed, bytes::Bytes::from_static(b"actual"))
        .await
        .unwrap_err();

    assert!(matches!(error, StorageError::DigestMismatch { .. }));
}

#[tokio::test]
async fn repository_namespaces_do_not_share_chunk_existence() {
    let chunks = ChunkStore::new(Arc::new(InMemory::new()));
    let first = Uuid::nil();
    let second = Uuid::from_u128(1);
    let content = bytes::Bytes::from_static(b"same bytes");
    let chunk_id = id(&content);

    chunks.put_verified(first, chunk_id, content).await.unwrap();

    assert!(chunks.exists(first, chunk_id).await.unwrap());
    assert!(!chunks.exists(second, chunk_id).await.unwrap());
}
