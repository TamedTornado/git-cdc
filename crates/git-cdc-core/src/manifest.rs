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
