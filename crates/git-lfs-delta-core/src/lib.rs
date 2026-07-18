//! Deterministic chunking and manifest primitives for Git LFS Delta.

mod digest;
mod manifest;
mod profile;
mod stream;

pub use digest::{ChunkId, ObjectOid};
pub use manifest::{
    ChunkDescriptor, MAX_CHUNK_DESCRIPTORS, MAX_OBJECT_SIZE, ManifestError, ManifestVersion,
    ObjectManifest,
};
pub use profile::ChunkingProfile;
pub use stream::{Chunk, ChunkStream, CoreError};
