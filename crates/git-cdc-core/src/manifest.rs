use serde::{Deserialize, Serialize};

use crate::{ChunkId, ChunkingProfile, ObjectOid};

/// Version of the serialized Git-CDC manifest contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ManifestVersion {
    /// First public manifest representation.
    #[serde(rename = "v1")]
    V1,
}

/// One ordered chunk in a logical Git LFS object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChunkDescriptor {
    /// BLAKE3 identity of the chunk bytes.
    pub id: ChunkId,
    /// Starting byte offset in the reconstructed logical object.
    pub offset: u64,
    /// Length of the chunk in bytes.
    pub length: u32,
}

/// A complete, ordered recipe for reconstructing a Git LFS object.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectManifest {
    /// Serialized manifest version.
    pub version: ManifestVersion,
    /// Deterministic chunking profile used to construct this manifest.
    pub profile: ChunkingProfile,
    /// Canonical SHA-256 object identity used by Git LFS.
    pub object_oid: ObjectOid,
    /// Total reconstructed object size in bytes.
    pub object_size: u64,
    /// Ordered chunk descriptors.
    pub chunks: Vec<ChunkDescriptor>,
}

impl ObjectManifest {
    /// Validates the structural invariants required before a manifest is trusted.
    ///
    /// This proves ordered, gap-free coverage of exactly `object_size`. Chunk
    /// bytes and the whole-object SHA-256 identity still require independent
    /// verification during finalization.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] for the first broken invariant.
    pub fn validate(&self) -> Result<(), ManifestError> {
        let mut expected_offset = 0_u64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            if chunk.length == 0 {
                return Err(ManifestError::EmptyChunk { index });
            }
            if chunk.offset != expected_offset {
                return Err(ManifestError::UnexpectedOffset {
                    index,
                    expected: expected_offset,
                    actual: chunk.offset,
                });
            }
            expected_offset = expected_offset
                .checked_add(u64::from(chunk.length))
                .ok_or(ManifestError::SizeOverflow)?;
        }
        if expected_offset != self.object_size {
            return Err(ManifestError::SizeMismatch {
                declared: self.object_size,
                described: expected_offset,
            });
        }
        Ok(())
    }
}

/// A structurally invalid object manifest.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ManifestError {
    /// A descriptor had no bytes.
    #[error("chunk {index} has zero length")]
    EmptyChunk {
        /// Zero-based descriptor index.
        index: usize,
    },
    /// A descriptor introduced a gap or overlap.
    #[error("chunk {index} starts at {actual}, expected {expected}")]
    UnexpectedOffset {
        /// Zero-based descriptor index.
        index: usize,
        /// Required contiguous offset.
        expected: u64,
        /// Supplied offset.
        actual: u64,
    },
    /// Descriptor lengths overflowed a 64-bit object size.
    #[error("manifest chunk lengths overflow the object-size representation")]
    SizeOverflow,
    /// Descriptor coverage did not equal the declared object size.
    #[error("manifest describes {described} bytes but declares {declared}")]
    SizeMismatch {
        /// Declared whole-object size.
        declared: u64,
        /// Sum of descriptor lengths.
        described: u64,
    },
}
