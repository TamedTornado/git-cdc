//! Behavioral contract shared by object-store providers.
#![allow(
    clippy::unwrap_used,
    reason = "storage contract setup should fail immediately"
)]

use std::sync::Arc;

use git_lfs_delta_core::ChunkId;
use git_lfs_delta_storage::{ChunkStore, StorageError};
use object_store::{
    ObjectStore, ObjectStoreExt, PutPayload, RetryConfig, memory::InMemory, path::Path,
};
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
    if std::env::var_os("GIT_LFS_DELTA_TEST_MINIO").is_none() {
        return;
    }
    let store = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name("git-lfs-delta-test")
        .with_region("us-east-1")
        .with_endpoint("http://127.0.0.1:59000")
        .with_access_key_id("git_lfs_delta")
        .with_secret_access_key("git_lfs_delta_secret")
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

#[tokio::test]
async fn corrupt_provider_bytes_are_never_returned_as_valid_chunks() {
    let provider = Arc::new(InMemory::new());
    let chunks = ChunkStore::new(provider.clone());
    let repository_id = Uuid::nil();
    let content = bytes::Bytes::from_static(b"valid immutable bytes");
    let chunk_id = id(&content);
    chunks
        .put_verified(repository_id, chunk_id, content)
        .await
        .unwrap();
    let digest = chunk_id.to_string();
    let path = Path::from(format!(
        "repositories/{repository_id}/chunks/{}/{digest}",
        &digest[..2]
    ));
    provider
        .put(
            &path,
            PutPayload::from(bytes::Bytes::from_static(b"corrupt provider bytes")),
        )
        .await
        .unwrap();

    assert!(matches!(
        chunks.get_verified(repository_id, chunk_id).await,
        Err(StorageError::DigestMismatch { .. })
    ));
}

#[tokio::test]
async fn unavailable_provider_is_reported_without_claiming_chunk_absence() {
    let provider = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name("unavailable")
        .with_region("us-east-1")
        .with_endpoint("http://127.0.0.1:9")
        .with_access_key_id("test")
        .with_secret_access_key("test")
        .with_allow_http(true)
        .with_retry(RetryConfig {
            max_retries: 0,
            ..RetryConfig::default()
        })
        .build()
        .unwrap();
    let chunks = ChunkStore::new(Arc::new(provider));

    assert!(matches!(
        chunks.exists(Uuid::nil(), id(b"not present")).await,
        Err(StorageError::Provider(_))
    ));
}
