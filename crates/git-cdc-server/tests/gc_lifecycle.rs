//! Safe reachability and garbage-collection lifecycle contracts.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::{io::Cursor, sync::Arc, time::Duration};

use git_cdc_core::{ChunkStream, ChunkingProfile, ObjectManifest};
use git_cdc_server::{
    gc::{
        ReachabilitySnapshot, collect_due, dry_run, reclaim_expired_uploads, stage, submit_snapshot,
    },
    migrate,
};
use git_cdc_storage::ChunkStore;
use object_store::memory::InMemory;
use sqlx::PgPool;
use uuid::Uuid;

async fn setup() -> (PgPool, ObjectManifest) {
    let url = std::env::var("GIT_CDC_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://git_cdc:git_cdc@127.0.0.1:55433/git_cdc".into());
    let pool = PgPool::connect(&url).await.unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query("TRUNCATE repositories CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories (id, owner, name) VALUES ($1, 'team', 'assets')")
        .bind(Uuid::nil())
        .execute(&pool)
        .await
        .unwrap();
    let manifest = ChunkStream::new(Cursor::new(Vec::<u8>::new()), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    sqlx::query("INSERT INTO objects (repository_id, oid, size, manifest) VALUES ($1, $2, 0, $3)")
        .bind(Uuid::nil())
        .bind(manifest.object_oid.as_bytes().as_slice())
        .bind(serde_json::to_value(&manifest).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    (pool, manifest)
}

#[tokio::test]
#[serial_test::serial]
async fn deletion_requires_two_complete_absent_snapshots_and_expired_grace() {
    let (pool, manifest) = setup().await;
    assert!(dry_run(&pool, Uuid::nil()).await.unwrap().is_empty());
    submit_snapshot(
        &pool,
        ReachabilitySnapshot {
            repository_id: Uuid::nil(),
            ref_fingerprint: "refs-1",
            reachable_objects: &[],
        },
    )
    .await
    .unwrap();
    assert!(dry_run(&pool, Uuid::nil()).await.unwrap().is_empty());
    submit_snapshot(
        &pool,
        ReachabilitySnapshot {
            repository_id: Uuid::nil(),
            ref_fingerprint: "refs-2",
            reachable_objects: &[],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        dry_run(&pool, Uuid::nil()).await.unwrap()[0].oid,
        manifest.object_oid
    );
    assert_eq!(
        stage(&pool, Uuid::nil(), Duration::ZERO)
            .await
            .unwrap()
            .len(),
        1
    );
    let chunks = ChunkStore::new(Arc::new(InMemory::new()));
    let (first, second) = tokio::join!(
        collect_due(&pool, &chunks, Uuid::nil()),
        collect_due(&pool, &chunks, Uuid::nil())
    );
    assert_eq!(first.unwrap() + second.unwrap(), 1);
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM objects WHERE repository_id = $1)")
            .bind(Uuid::nil())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!exists);
}

#[tokio::test]
#[serial_test::serial]
async fn any_reachability_in_the_two_snapshot_window_prevents_deletion() {
    let (pool, manifest) = setup().await;
    submit_snapshot(
        &pool,
        ReachabilitySnapshot {
            repository_id: Uuid::nil(),
            ref_fingerprint: "refs-1",
            reachable_objects: &[],
        },
    )
    .await
    .unwrap();
    submit_snapshot(
        &pool,
        ReachabilitySnapshot {
            repository_id: Uuid::nil(),
            ref_fingerprint: "refs-2",
            reachable_objects: &[manifest.object_oid],
        },
    )
    .await
    .unwrap();
    assert!(dry_run(&pool, Uuid::nil()).await.unwrap().is_empty());
}

#[tokio::test]
#[serial_test::serial]
async fn an_interrupted_chunk_deletion_queue_is_drained_on_retry() {
    let (pool, _) = setup().await;
    let content = bytes::Bytes::from(vec![0x6b_u8; 600_000]);
    let manifest = ChunkStream::new(Cursor::new(&content), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    let descriptor = &manifest.chunks[0];
    let provider = Arc::new(InMemory::new());
    let chunks = ChunkStore::new(provider);
    chunks
        .put_verified(Uuid::nil(), descriptor.id, content)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO chunks (repository_id, chunk_id, size, reference_count) \
         VALUES ($1, $2, $3, 0)",
    )
    .bind(Uuid::nil())
    .bind(descriptor.id.as_bytes().as_slice())
    .bind(i64::from(descriptor.length))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO chunk_gc_queue (repository_id, chunk_id) VALUES ($1, $2)")
        .bind(Uuid::nil())
        .bind(descriptor.id.as_bytes().as_slice())
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(collect_due(&pool, &chunks, Uuid::nil()).await.unwrap(), 0);
    assert!(!chunks.exists(Uuid::nil(), descriptor.id).await.unwrap());
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM chunk_gc_queue")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
}

#[tokio::test]
#[serial_test::serial]
async fn a_queued_chunk_that_becomes_referenced_is_not_deleted() {
    let (pool, _) = setup().await;
    let content = bytes::Bytes::from(vec![0x4a_u8; 600_000]);
    let manifest = ChunkStream::new(Cursor::new(&content), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    let descriptor = &manifest.chunks[0];
    let chunks = ChunkStore::new(Arc::new(InMemory::new()));
    chunks
        .put_verified(Uuid::nil(), descriptor.id, content)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO chunks (repository_id, chunk_id, size, reference_count) \
         VALUES ($1, $2, $3, 1)",
    )
    .bind(Uuid::nil())
    .bind(descriptor.id.as_bytes().as_slice())
    .bind(i64::from(descriptor.length))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO chunk_gc_queue (repository_id, chunk_id) VALUES ($1, $2)")
        .bind(Uuid::nil())
        .bind(descriptor.id.as_bytes().as_slice())
        .execute(&pool)
        .await
        .unwrap();

    collect_due(&pool, &chunks, Uuid::nil()).await.unwrap();

    assert!(chunks.exists(Uuid::nil(), descriptor.id).await.unwrap());
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM chunk_gc_queue")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
}

#[tokio::test]
#[serial_test::serial]
async fn expired_partial_uploads_are_quarantined_then_reclaimed_without_touching_live_data() {
    let (pool, _) = setup().await;
    let content = bytes::Bytes::from(vec![0x2d_u8; 600_000]);
    let manifest = ChunkStream::new(Cursor::new(&content), ChunkingProfile::beta_v1())
        .finish()
        .unwrap();
    let descriptor = &manifest.chunks[0];
    let session_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO upload_sessions \
         (id, repository_id, object_oid, object_size, manifest, state, expires_at) \
         VALUES ($1, $2, $3, $4, $5, 'expired', now() - interval '1 day')",
    )
    .bind(session_id)
    .bind(Uuid::nil())
    .bind(manifest.object_oid.as_bytes().as_slice())
    .bind(i64::try_from(manifest.object_size).unwrap())
    .bind(serde_json::to_value(&manifest).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let active_session = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO upload_sessions \
         (id, repository_id, object_oid, object_size, manifest, state, expires_at) \
         VALUES ($1, $2, $3, $4, $5, 'open', now() + interval '1 day')",
    )
    .bind(active_session)
    .bind(Uuid::nil())
    .bind(manifest.object_oid.as_bytes().as_slice())
    .bind(i64::try_from(manifest.object_size).unwrap())
    .bind(serde_json::to_value(&manifest).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO chunks (repository_id, chunk_id, size, reference_count) \
         VALUES ($1, $2, $3, 0)",
    )
    .bind(Uuid::nil())
    .bind(descriptor.id.as_bytes().as_slice())
    .bind(i64::from(descriptor.length))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO upload_session_chunks (session_id, repository_id, chunk_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(active_session)
    .bind(Uuid::nil())
    .bind(descriptor.id.as_bytes().as_slice())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO upload_session_chunks (session_id, repository_id, chunk_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(session_id)
    .bind(Uuid::nil())
    .bind(descriptor.id.as_bytes().as_slice())
    .execute(&pool)
    .await
    .unwrap();
    let chunks = ChunkStore::new(Arc::new(InMemory::new()));
    chunks
        .put_verified(Uuid::nil(), descriptor.id, content)
        .await
        .unwrap();

    assert_eq!(
        reclaim_expired_uploads(&pool, &chunks, Uuid::nil(), Duration::ZERO)
            .await
            .unwrap(),
        1
    );
    assert!(chunks.exists(Uuid::nil(), descriptor.id).await.unwrap());
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM upload_sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 1);

    sqlx::query("UPDATE upload_sessions SET expires_at = now() - interval '1 day'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        reclaim_expired_uploads(&pool, &chunks, Uuid::nil(), Duration::ZERO)
            .await
            .unwrap(),
        1
    );
    assert!(!chunks.exists(Uuid::nil(), descriptor.id).await.unwrap());
}
