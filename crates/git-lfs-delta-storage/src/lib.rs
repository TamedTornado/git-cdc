//! Provider-neutral immutable chunk storage for Git LFS Delta.

use std::sync::Arc;

use bytes::Bytes;
use git_lfs_delta_core::ChunkId;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload, path::Path};
use uuid::Uuid;

/// Repository-scoped immutable chunk operations over any supported provider.
#[derive(Clone)]
pub struct ChunkStore {
    inner: Arc<dyn ObjectStore>,
}

impl ChunkStore {
    /// Wraps an object-store implementation with Git LFS Delta integrity semantics.
    #[must_use]
    pub fn new(inner: Arc<dyn ObjectStore>) -> Self {
        Self { inner }
    }

    /// Atomically stores bytes only if they match the claimed chunk identity.
    ///
    /// Repeating the same write is successful. The repository is part of the
    /// physical key so existence cannot leak across repository boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] for an integrity failure or provider error.
    pub async fn put_verified(
        &self,
        repository_id: Uuid,
        claimed_id: ChunkId,
        bytes: Bytes,
    ) -> Result<(), StorageError> {
        verify_digest(claimed_id, &bytes)?;
        let path = chunk_path(repository_id, claimed_id);
        let options = PutOptions {
            mode: PutMode::Create,
            ..PutOptions::default()
        };
        match self
            .inner
            .put_opts(&path, PutPayload::from(bytes), options)
            .await
        {
            Ok(_) | Err(object_store::Error::AlreadyExists { .. }) => Ok(()),
            Err(error) => Err(StorageError::Provider(error)),
        }
    }

    /// Reads a chunk and verifies that the provider returned the requested bytes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the chunk is absent, corrupt, or unreadable.
    pub async fn get_verified(
        &self,
        repository_id: Uuid,
        chunk_id: ChunkId,
    ) -> Result<Bytes, StorageError> {
        let bytes = self
            .inner
            .get(&chunk_path(repository_id, chunk_id))
            .await
            .map_err(StorageError::Provider)?
            .bytes()
            .await
            .map_err(StorageError::Provider)?;
        verify_digest(chunk_id, &bytes)?;
        Ok(bytes)
    }

    /// Reports whether a chunk exists inside one repository namespace.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the provider cannot answer reliably.
    pub async fn exists(
        &self,
        repository_id: Uuid,
        chunk_id: ChunkId,
    ) -> Result<bool, StorageError> {
        match self.inner.head(&chunk_path(repository_id, chunk_id)).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(error) => Err(StorageError::Provider(error)),
        }
    }

    /// Idempotently deletes one repository-scoped chunk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the provider fails for a reason other than
    /// the chunk already being absent.
    pub async fn delete(&self, repository_id: Uuid, chunk_id: ChunkId) -> Result<(), StorageError> {
        match self
            .inner
            .delete(&chunk_path(repository_id, chunk_id))
            .await
        {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(StorageError::Provider(error)),
        }
    }

    /// Performs a provider-neutral write/read/delete readiness probe.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when any operation fails or returns unexpected
    /// bytes. Probe objects use a reserved prefix and are removed on success.
    pub async fn healthcheck(&self) -> Result<(), StorageError> {
        let path = Path::from(format!("_health/{}", Uuid::new_v4()));
        let expected = Bytes::from_static(b"git-lfs-delta-ready");
        self.inner
            .put(&path, PutPayload::from(expected.clone()))
            .await
            .map_err(StorageError::Provider)?;
        let actual = self
            .inner
            .get(&path)
            .await
            .map_err(StorageError::Provider)?
            .bytes()
            .await
            .map_err(StorageError::Provider)?;
        self.inner
            .delete(&path)
            .await
            .map_err(StorageError::Provider)?;
        if actual == expected {
            Ok(())
        } else {
            Err(StorageError::ProbeMismatch)
        }
    }
}

/// Integrity or provider failure while accessing immutable chunks.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// A readiness probe read bytes different from those just written.
    #[error("object storage readiness probe returned unexpected bytes")]
    ProbeMismatch,
    /// Bytes did not hash to their claimed BLAKE3 identity.
    #[error("chunk digest mismatch: expected {expected}, received {actual}")]
    DigestMismatch {
        /// Identity requested by the caller or manifest.
        expected: ChunkId,
        /// BLAKE3 identity calculated from the supplied bytes.
        actual: String,
    },
    /// The configured object-store provider failed.
    #[error("object storage failed: {0}")]
    Provider(#[source] object_store::Error),
}

fn verify_digest(expected: ChunkId, bytes: &[u8]) -> Result<(), StorageError> {
    let actual = blake3::hash(bytes);
    if actual.as_bytes() == expected.as_bytes() {
        Ok(())
    } else {
        Err(StorageError::DigestMismatch {
            expected,
            actual: actual.to_hex().to_string(),
        })
    }
}

fn chunk_path(repository_id: Uuid, chunk_id: ChunkId) -> Path {
    let digest = chunk_id.to_string();
    Path::from(format!(
        "repositories/{repository_id}/chunks/{}/{digest}",
        &digest[..2]
    ))
}
