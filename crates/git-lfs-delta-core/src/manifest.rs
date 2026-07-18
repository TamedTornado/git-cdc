use serde::{Deserialize, Serialize};

use crate::{ChunkId, ChunkingProfile, ObjectOid};

/// Largest logical object accepted by the beta production profile (100 GiB).
pub const MAX_OBJECT_SIZE: u64 = 100 * 1024 * 1024 * 1024;

/// Maximum number of descriptors possible at the beta profile's minimum size.
pub const MAX_CHUNK_DESCRIPTORS: usize = 204_800;

/// Version of the serialized Git LFS Delta manifest contract.
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
        if self.object_size > MAX_OBJECT_SIZE {
            return Err(ManifestError::ObjectTooLarge {
                actual: self.object_size,
                maximum: MAX_OBJECT_SIZE,
            });
        }
        if self.chunks.len() > MAX_CHUNK_DESCRIPTORS {
            return Err(ManifestError::TooManyChunks {
                actual: self.chunks.len(),
                maximum: MAX_CHUNK_DESCRIPTORS,
            });
        }
        let (minimum, _, maximum) = self.profile.sizes();
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
            if chunk.length as usize > maximum {
                return Err(ManifestError::ChunkTooLarge {
                    index,
                    actual: chunk.length,
                    maximum: u32::try_from(maximum).unwrap_or(u32::MAX),
                });
            }
            if index + 1 < self.chunks.len() && (chunk.length as usize) < minimum {
                return Err(ManifestError::ChunkTooSmall {
                    index,
                    actual: chunk.length,
                    minimum: u32::try_from(minimum).unwrap_or(u32::MAX),
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
    /// The logical object exceeds the production profile's resource limit.
    #[error("object size {actual} exceeds the maximum {maximum}")]
    ObjectTooLarge {
        /// Declared logical size.
        actual: u64,
        /// Configured protocol maximum.
        maximum: u64,
    },
    /// The manifest contains too many database-amplifying descriptors.
    #[error("manifest has {actual} chunks, exceeding the maximum {maximum}")]
    TooManyChunks {
        /// Submitted descriptor count.
        actual: usize,
        /// Maximum descriptor count.
        maximum: usize,
    },
    /// A descriptor had no bytes.
    #[error("chunk {index} has zero length")]
    EmptyChunk {
        /// Zero-based descriptor index.
        index: usize,
    },
    /// A non-final chunk is smaller than the selected profile permits.
    #[error("chunk {index} has length {actual}, below the minimum {minimum}")]
    ChunkTooSmall {
        /// Zero-based descriptor index.
        index: usize,
        /// Submitted length.
        actual: u32,
        /// Profile minimum.
        minimum: u32,
    },
    /// A chunk is larger than the selected profile permits.
    #[error("chunk {index} has length {actual}, above the maximum {maximum}")]
    ChunkTooLarge {
        /// Zero-based descriptor index.
        index: usize,
        /// Submitted length.
        actual: u32,
        /// Profile maximum.
        maximum: u32,
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
