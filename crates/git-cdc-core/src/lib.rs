//! Deterministic chunking and manifest primitives for Git-CDC.

mod digest;
mod manifest;
mod profile;
mod stream;

pub use digest::{ChunkId, ObjectOid};
pub use manifest::{ChunkDescriptor, ManifestError, ManifestVersion, ObjectManifest};
pub use profile::ChunkingProfile;
pub use stream::{Chunk, ChunkStream, CoreError};
