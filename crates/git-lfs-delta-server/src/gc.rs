//! Conservative reachability snapshots and crash-retryable garbage collection.

use std::time::Duration;

use git_lfs_delta_core::{ChunkId, ObjectOid};
use git_lfs_delta_storage::ChunkStore;
use sqlx::PgPool;
use uuid::Uuid;

/// One completed, immutable reachability observation.
pub struct ReachabilitySnapshot<'a> {
    /// Repository whose ordinary Git refs were scanned.
    pub repository_id: Uuid,
    /// Stable fingerprint of the exact ref tips scanned.
    pub ref_fingerprint: &'a str,
    /// Every LFS OID reachable from branches and tags in that scan.
    pub reachable_objects: &'a [ObjectOid],
}

/// One logical object eligible for staging after consecutive complete snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GcCandidate {
    /// Canonical LFS object identity.
    pub oid: ObjectOid,
    /// Logical object size.
    pub size: u64,
}

/// Records one complete reachability epoch atomically.
///
/// # Errors
///
/// Returns a database error without publishing a partial epoch.
pub async fn submit_snapshot(
    pool: &PgPool,
    snapshot: ReachabilitySnapshot<'_>,
) -> Result<Uuid, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let epoch = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO reachability_epochs (id, repository_id, ref_fingerprint, complete) \
         VALUES ($1, $2, $3, false)",
    )
    .bind(epoch)
    .bind(snapshot.repository_id)
    .bind(snapshot.ref_fingerprint)
    .execute(&mut *transaction)
    .await?;
    for oid in snapshot.reachable_objects {
        sqlx::query("INSERT INTO reachable_objects (epoch_id, object_oid) VALUES ($1, $2)")
            .bind(epoch)
            .bind(oid.as_bytes().as_slice())
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query(
        "UPDATE reachability_epochs SET complete = true, completed_at = now() WHERE id = $1",
    )
    .bind(epoch)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(epoch)
}

/// Returns objects absent from the two newest complete snapshots.
///
/// No complete source, only one snapshot, incomplete epochs, and already
/// staged objects all produce no deletion proposal.
///
/// # Errors
///
/// Returns a database error if candidate proof cannot be established.
pub async fn dry_run(pool: &PgPool, repository_id: Uuid) -> Result<Vec<GcCandidate>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (Vec<u8>, i64)>(
        "WITH latest AS ( \
             SELECT id FROM reachability_epochs \
             WHERE repository_id = $1 AND complete = true \
             ORDER BY completed_at DESC, id DESC LIMIT 2 \
         ), proof AS (SELECT count(*) AS epochs FROM latest) \
         SELECT o.oid, o.size FROM objects o CROSS JOIN proof p \
         WHERE o.repository_id = $1 AND p.epochs = 2 \
           AND NOT EXISTS ( \
             SELECT 1 FROM reachable_objects ro JOIN latest l ON l.id = ro.epoch_id \
             WHERE ro.object_oid = o.oid \
           ) \
           AND NOT EXISTS ( \
             SELECT 1 FROM object_tombstones t \
             WHERE t.repository_id = o.repository_id AND t.object_oid = o.oid \
           ) ORDER BY o.oid",
    )
    .bind(repository_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(oid, size)| {
            let oid = oid
                .as_slice()
                .try_into()
                .map(ObjectOid::from_bytes)
                .map_err(|_| sqlx::Error::Protocol("invalid object OID in database".into()))?;
            let size = u64::try_from(size)
                .map_err(|_| sqlx::Error::Protocol("invalid object size in database".into()))?;
            Ok(GcCandidate { oid, size })
        })
        .collect()
}

/// Stages currently proven candidates behind an explicit grace period.
///
/// # Errors
///
/// Returns a database error without weakening snapshot requirements.
pub async fn stage(
    pool: &PgPool,
    repository_id: Uuid,
    grace: Duration,
) -> Result<Vec<GcCandidate>, sqlx::Error> {
    let candidates = dry_run(pool, repository_id).await?;
    let seconds = i64::try_from(grace.as_secs())
        .map_err(|_| sqlx::Error::Protocol("GC grace period is too large".into()))?;
    let mut transaction = pool.begin().await?;
    for candidate in &candidates {
        sqlx::query(
            "INSERT INTO object_tombstones (repository_id, object_oid, delete_after) \
             VALUES ($1, $2, now() + make_interval(secs => $3)) ON CONFLICT DO NOTHING",
        )
        .bind(repository_id)
        .bind(candidate.oid.as_bytes().as_slice())
        .bind(seconds)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(candidates)
}

/// Reclaims due tombstones and retries provider deletions from a durable queue.
///
/// # Errors
///
/// Returns database or provider errors; queued chunk deletions remain durable
/// and safe to retry after interruption.
pub async fn collect_due(
    pool: &PgPool,
    chunks: &ChunkStore,
    repository_id: Uuid,
) -> Result<u64, GcError> {
    let mut deleted = 0_u64;
    loop {
        let mut transaction = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(repository_id)
            .execute(&mut *transaction)
            .await?;
        let oid: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT object_oid FROM object_tombstones \
             WHERE repository_id = $1 AND delete_after <= now() \
             ORDER BY delete_after FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .bind(repository_id)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(oid) = oid else {
            transaction.commit().await?;
            break;
        };
        let chunk_ids: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT chunk_id FROM object_chunks WHERE repository_id = $1 AND object_oid = $2",
        )
        .bind(repository_id)
        .bind(&oid)
        .fetch_all(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM objects WHERE repository_id = $1 AND oid = $2")
            .bind(repository_id)
            .bind(&oid)
            .execute(&mut *transaction)
            .await?;
        for chunk_id in chunk_ids {
            sqlx::query(
                "UPDATE chunks SET reference_count = reference_count - 1 \
                 WHERE repository_id = $1 AND chunk_id = $2",
            )
            .bind(repository_id)
            .bind(&chunk_id)
            .execute(&mut *transaction)
            .await?;
            sqlx::query(
                "INSERT INTO chunk_gc_queue (repository_id, chunk_id) \
                 SELECT repository_id, chunk_id FROM chunks \
                 WHERE repository_id = $1 AND chunk_id = $2 AND reference_count = 0 \
                 ON CONFLICT DO NOTHING",
            )
            .bind(repository_id)
            .bind(&chunk_id)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        deleted += 1;
    }
    drain_chunk_queue(pool, chunks, repository_id).await?;
    Ok(deleted)
}

/// Expires abandoned uploads and reclaims their unreferenced chunks after a grace period.
///
/// # Errors
///
/// Returns database or provider errors. Provider failures leave durable queue
/// entries which a later invocation safely retries.
pub async fn reclaim_expired_uploads(
    pool: &PgPool,
    chunks: &ChunkStore,
    repository_id: Uuid,
    grace: Duration,
) -> Result<u64, GcError> {
    let seconds = i64::try_from(grace.as_secs())
        .map_err(|_| sqlx::Error::Protocol("upload grace period is too large".into()))?;
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "UPDATE upload_sessions SET state = 'expired' \
         WHERE repository_id = $1 AND state = 'open' AND expires_at <= now()",
    )
    .bind(repository_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO chunk_gc_queue (repository_id, chunk_id) \
         SELECT DISTINCT c.repository_id, c.chunk_id \
         FROM chunks c \
         JOIN upload_session_chunks usc \
           ON usc.repository_id = c.repository_id AND usc.chunk_id = c.chunk_id \
         JOIN upload_sessions expired ON expired.id = usc.session_id \
         WHERE c.repository_id = $1 AND c.reference_count = 0 \
           AND expired.state = 'expired' \
           AND expired.expires_at <= now() - make_interval(secs => $2) \
           AND NOT EXISTS ( \
             SELECT 1 FROM upload_session_chunks active_chunks \
             JOIN upload_sessions active ON active.id = active_chunks.session_id \
             WHERE active_chunks.repository_id = c.repository_id \
               AND active_chunks.chunk_id = c.chunk_id AND active.state = 'open' \
           ) ON CONFLICT DO NOTHING",
    )
    .bind(repository_id)
    .bind(seconds)
    .execute(&mut *transaction)
    .await?;
    let removed = sqlx::query(
        "DELETE FROM upload_sessions WHERE repository_id = $1 AND state = 'expired' \
         AND expires_at <= now() - make_interval(secs => $2)",
    )
    .bind(repository_id)
    .bind(seconds)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    drain_chunk_queue(pool, chunks, repository_id).await?;
    Ok(removed)
}

async fn drain_chunk_queue(
    pool: &PgPool,
    chunks: &ChunkStore,
    repository_id: Uuid,
) -> Result<(), GcError> {
    let queued: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT chunk_id FROM chunk_gc_queue WHERE repository_id = $1 ORDER BY queued_at",
    )
    .bind(repository_id)
    .fetch_all(pool)
    .await?;
    for raw in queued {
        let mut transaction = pool.begin().await?;
        let reference_count: Option<i64> = sqlx::query_scalar(
            "SELECT reference_count FROM chunks \
             WHERE repository_id = $1 AND chunk_id = $2 FOR UPDATE",
        )
        .bind(repository_id)
        .bind(&raw)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(reference_count) = reference_count else {
            sqlx::query("DELETE FROM chunk_gc_queue WHERE repository_id = $1 AND chunk_id = $2")
                .bind(repository_id)
                .bind(&raw)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            continue;
        };
        if reference_count > 0 {
            sqlx::query("DELETE FROM chunk_gc_queue WHERE repository_id = $1 AND chunk_id = $2")
                .bind(repository_id)
                .bind(&raw)
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            continue;
        }
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS( \
               SELECT 1 FROM upload_session_chunks usc \
               JOIN upload_sessions s ON s.id = usc.session_id \
               WHERE usc.repository_id = $1 AND usc.chunk_id = $2 AND s.state = 'open' \
             )",
        )
        .bind(repository_id)
        .bind(&raw)
        .fetch_one(&mut *transaction)
        .await?;
        if active {
            transaction.commit().await?;
            continue;
        }
        let bytes: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| GcError::CorruptChunkId)?;
        let chunk_id = ChunkId::from_bytes(bytes);
        chunks.delete(repository_id, chunk_id).await?;
        sqlx::query(
            "DELETE FROM chunks WHERE repository_id = $1 AND chunk_id = $2 AND reference_count = 0",
        )
        .bind(repository_id)
        .bind(&raw)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM chunk_gc_queue WHERE repository_id = $1 AND chunk_id = $2")
            .bind(repository_id)
            .bind(&raw)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
    }
    Ok(())
}

/// Safe-GC database, integrity, or storage failure.
#[derive(Debug, thiserror::Error)]
pub enum GcError {
    /// Metadata operation failed.
    #[error("GC database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    /// Provider deletion failed and remains queued for retry.
    #[error("GC object-store operation failed: {0}")]
    Storage(#[from] git_lfs_delta_storage::StorageError),
    /// Persistent metadata contained a malformed chunk identity.
    #[error("GC metadata contains an invalid chunk identity")]
    CorruptChunkId,
}
