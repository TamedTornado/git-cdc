//! Safe reachability and garbage-collection lifecycle contracts.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::{io::Cursor, sync::Arc, time::Duration};

use git_cdc_core::{ChunkStream, ChunkingProfile, ObjectManifest};
use git_cdc_server::{
    gc::{ReachabilitySnapshot, collect_due, dry_run, stage, submit_snapshot},
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
    assert_eq!(collect_due(&pool, &chunks, Uuid::nil()).await.unwrap(), 1);
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
