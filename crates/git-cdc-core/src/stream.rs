use std::io::Read;

use fastcdc::v2020;
use sha2::{Digest, Sha256};

use crate::{
    ChunkDescriptor, ChunkId, ChunkingProfile, ManifestVersion, ObjectManifest, ObjectOid,
};

/// One bounded piece of streamed object data and its verified descriptor.
#[derive(Debug, Eq, PartialEq)]
pub struct Chunk {
    /// Metadata committed to the object manifest.
    pub descriptor: ChunkDescriptor,
    /// Source bytes for upload, reconstruction, or local caching.
    pub data: Vec<u8>,
}

/// Failure while streaming or describing an object.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// `FastCDC` could not read or partition the source.
    #[error("content-defined chunking failed: {0}")]
    Chunking(String),
    /// A chunk exceeded the representable v1 manifest length.
    #[error("chunk length {0} exceeds the v1 manifest limit")]
    ChunkTooLarge(usize),
}

/// A bounded-memory iterator that chunks a reader and accumulates its manifest.
pub struct ChunkStream<R: Read> {
    inner: v2020::StreamCDC<R>,
    profile: ChunkingProfile,
    object_hasher: Sha256,
    descriptors: Vec<ChunkDescriptor>,
    object_size: u64,
}

impl<R: Read> ChunkStream<R> {
    /// Creates a stream using an explicit, versioned chunking profile.
    #[must_use]
    pub fn new(reader: R, profile: ChunkingProfile) -> Self {
        let (minimum, average, maximum) = profile.sizes();
        Self {
            inner: v2020::StreamCDC::new(reader, minimum, average, maximum),
            profile,
            object_hasher: Sha256::new(),
            descriptors: Vec::new(),
            object_size: 0,
        }
    }

    /// Drains any unread chunks and returns the complete object manifest.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when the source cannot be read or a chunk cannot
    /// be represented by the selected manifest version.
    pub fn finish(mut self) -> Result<ObjectManifest, CoreError> {
        for chunk in self.by_ref() {
            chunk?;
        }
        Ok(self.completed_manifest())
    }

    /// Drains the stream without retaining chunk bytes and returns its manifest.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when the source cannot be read or a chunk cannot
    /// be represented by the selected manifest version.
    pub fn collect_manifest(self) -> Result<ObjectManifest, CoreError> {
        self.finish()
    }

    fn completed_manifest(self) -> ObjectManifest {
        let object_oid = ObjectOid::from_bytes(self.object_hasher.finalize().into());
        ObjectManifest {
            version: ManifestVersion::V1,
            profile: self.profile,
            object_oid,
            object_size: self.object_size,
            chunks: self.descriptors,
        }
    }
}

impl<R: Read> Iterator for ChunkStream<R> {
    type Item = Result<Chunk, CoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let chunk = match self.inner.next()? {
            Ok(chunk) => chunk,
            Err(error) => return Some(Err(CoreError::Chunking(error.to_string()))),
        };
        let Ok(length) = u32::try_from(chunk.length) else {
            return Some(Err(CoreError::ChunkTooLarge(chunk.length)));
        };
        self.object_hasher.update(&chunk.data);
        self.object_size += u64::from(length);
        let descriptor = ChunkDescriptor {
            id: ChunkId::from_bytes(blake3::hash(&chunk.data).into()),
            offset: chunk.offset,
            length,
        };
        self.descriptors.push(descriptor.clone());
        Some(Ok(Chunk {
            descriptor,
            data: chunk.data,
        }))
    }
}
