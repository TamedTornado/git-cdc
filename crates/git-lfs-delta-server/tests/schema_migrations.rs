//! Upgrade coverage from the schema shipped immediately before beta.2 hardening.
#![allow(clippy::unwrap_used, reason = "database fixtures fail immediately")]

use std::borrow::Cow;

use git_lfs_delta_server::{MIGRATOR, SchemaError, migrate, schema_check, schema_status};
use serde_json::json;
use sqlx::{PgPool, migrate::Migrator};
use uuid::Uuid;

#[tokio::test]
#[serial_test::serial]
async fn populated_previous_schema_upgrades_without_data_loss() {
    let database_url = std::env::var("GIT_LFS_DELTA_TEST_DATABASE_URL").unwrap_or_else(|_| {
        "postgres://git_lfs_delta:git_lfs_delta@127.0.0.1:55433/git_lfs_delta".into()
    });
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .execute(&pool)
        .await
        .unwrap();

    let previous = Migrator {
        migrations: Cow::Owned(MIGRATOR.iter().take(4).cloned().collect()),
        ..Migrator::DEFAULT
    };
    previous.run(&pool).await.unwrap();

    let repository_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    let object_oid = vec![2_u8; 32];
    let chunk_id = vec![3_u8; 32];
    sqlx::query("INSERT INTO repositories (id, owner, name) VALUES ($1, 'team', 'assets')")
        .bind(repository_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO chunks (repository_id, chunk_id, size) VALUES ($1, $2, 1048576)")
        .bind(repository_id)
        .bind(&chunk_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO upload_sessions \
         (id, repository_id, object_oid, object_size, manifest, state, expires_at) \
         VALUES ($1, $2, $3, 1048576, $4, 'open', now() + interval '1 hour')",
    )
    .bind(session_id)
    .bind(repository_id)
    .bind(&object_oid)
    .bind(json!({"fixture": "previous-release"}))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO upload_session_chunks (session_id, repository_id, chunk_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(session_id)
    .bind(repository_id)
    .bind(&chunk_id)
    .execute(&pool)
    .await
    .unwrap();

    let before = schema_status(&pool).await.unwrap();
    assert_eq!(before.pending, 1);
    assert!(matches!(
        schema_check(&pool).await,
        Err(SchemaError::Pending { pending: 1 })
    ));
    migrate(&pool).await.unwrap();
    let after = schema_check(&pool).await.unwrap();
    assert_eq!(after.pending, 0);
    assert_eq!(after.applied_version, after.target_version);

    let session_chunks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM upload_session_chunks \
         WHERE session_id = $1 AND repository_id = $2 AND chunk_id = $3",
    )
    .bind(session_id)
    .bind(repository_id)
    .bind(&chunk_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(session_chunks, 1);

    let other_repository_id = Uuid::new_v4();
    sqlx::query("INSERT INTO repositories (id, owner, name) VALUES ($1, 'team', 'other')")
        .bind(other_repository_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO chunks (repository_id, chunk_id, size) VALUES ($1, $2, 1048576)")
        .bind(other_repository_id)
        .bind(&chunk_id)
        .execute(&pool)
        .await
        .unwrap();
    let cross_repository_insert = sqlx::query(
        "INSERT INTO upload_session_chunks (session_id, repository_id, chunk_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(session_id)
    .bind(other_repository_id)
    .bind(&chunk_id)
    .execute(&pool)
    .await;
    assert!(cross_repository_insert.is_err());
}
