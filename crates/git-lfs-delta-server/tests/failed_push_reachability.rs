//! Reachability contract for an LFS upload whose Git push never arrived.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::{fs, io::Cursor, process::Command};

use git_lfs_delta_core::{ChunkStream, ChunkingProfile};
use git_lfs_delta_server::{
    gc::{ReachabilitySnapshot, dry_run, submit_snapshot},
    migrate,
    reconcile::scan,
};
use sqlx::PgPool;
use uuid::Uuid;

fn git(repository: &std::path::Path, arguments: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .status()
            .unwrap()
            .success()
    );
}

#[tokio::test]
#[serial_test::serial]
async fn completed_lfs_upload_without_a_git_ref_requires_two_absent_scans() {
    let database_url = std::env::var("GIT_LFS_DELTA_TEST_DATABASE_URL").unwrap_or_else(|_| {
        "postgres://git_lfs_delta:git_lfs_delta@127.0.0.1:55433/git_lfs_delta".into()
    });
    let pool = PgPool::connect(&database_url).await.unwrap();
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
    let manifest = ChunkStream::new(
        Cursor::new(vec![0x7e_u8; 600_000]),
        ChunkingProfile::beta_v1(),
    )
    .finish()
    .unwrap();
    sqlx::query("INSERT INTO objects (repository_id, oid, size, manifest) VALUES ($1, $2, $3, $4)")
        .bind(Uuid::nil())
        .bind(manifest.object_oid.as_bytes().as_slice())
        .bind(i64::try_from(manifest.object_size).unwrap())
        .bind(serde_json::to_value(&manifest).unwrap())
        .execute(&pool)
        .await
        .unwrap();

    let repository = tempfile::tempdir().unwrap();
    git(repository.path(), &["init", "-b", "master"]);
    git(repository.path(), &["config", "user.name", "Git CDC Test"]);
    git(
        repository.path(),
        &["config", "user.email", "test@git-lfs-delta.invalid"],
    );
    fs::write(
        repository.path().join("README.md"),
        "Git push without LFS pointer\n",
    )
    .unwrap();
    git(repository.path(), &["add", "README.md"]);
    git(repository.path(), &["commit", "-m", "reachable Git state"]);

    for observation in 1..=2 {
        let scan = scan(repository.path().to_str().unwrap()).unwrap();
        assert!(scan.reachable_objects.is_empty());
        submit_snapshot(
            &pool,
            ReachabilitySnapshot {
                repository_id: Uuid::nil(),
                ref_fingerprint: &scan.ref_fingerprint,
                reachable_objects: &scan.reachable_objects,
            },
        )
        .await
        .unwrap();
        let candidates = dry_run(&pool, Uuid::nil()).await.unwrap();
        if observation == 1 {
            assert!(candidates.is_empty());
        } else {
            assert_eq!(candidates[0].oid, manifest.object_oid);
        }
    }
}
