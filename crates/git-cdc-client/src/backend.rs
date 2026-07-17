//! Bounded-memory HTTP implementation of the CDC transfer data plane.

use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
};

use git_cdc_core::{ChunkStream, ChunkingProfile, ObjectManifest};
use git_cdc_protocol::{BeginUploadRequest, BeginUploadResponse};
use reqwest::blocking::{Client, RequestBuilder};
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    BackendError, DownloadRequest, DownloadResult, TransferAction, TransferBackend, UploadRequest,
};

/// Production HTTP backend used by the Git LFS custom-transfer process.
pub struct HttpBackend {
    client: Client,
    cache_root: PathBuf,
}

impl HttpBackend {
    /// Creates a backend with an explicit cross-platform cache directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(cache_root: PathBuf) -> Result<Self, BackendError> {
        let client = Client::builder()
            .build()
            .map_err(|error| failure(format!("could not create HTTP client: {error}")))?;
        Ok(Self { client, cache_root })
    }

    fn action(action: Option<&TransferAction>) -> Result<&TransferAction, BackendError> {
        action.ok_or_else(|| failure("server did not provide a CDC transfer action"))
    }

    fn authorized(builder: RequestBuilder, action: &TransferAction) -> RequestBuilder {
        action
            .header
            .iter()
            .fold(builder, |request, (name, value)| {
                request.header(name, value)
            })
    }

    fn chunk_cache_path(&self, digest: &str) -> PathBuf {
        self.cache_root
            .join("chunks")
            .join(&digest[..2])
            .join(digest)
    }

    fn read_or_fetch_chunk(
        &self,
        action: &TransferAction,
        index: usize,
        descriptor: &git_cdc_core::ChunkDescriptor,
    ) -> Result<Vec<u8>, BackendError> {
        let digest = descriptor.id.to_string();
        let path = self.chunk_cache_path(&digest);
        if let Ok(bytes) = fs::read(&path) {
            if valid_chunk(&bytes, descriptor) {
                return Ok(bytes);
            }
            fs::remove_file(&path).map_err(|error| {
                failure(format!("could not remove corrupt cache entry: {error}"))
            })?;
        }
        let url = child_url(&action.href, &["chunks", &index.to_string()])?;
        let response = Self::authorized(self.client.get(url), action)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| failure(format!("chunk download failed: {error}")))?;
        let bytes = response
            .bytes()
            .map_err(|error| failure(format!("could not read chunk response: {error}")))?
            .to_vec();
        if !valid_chunk(&bytes, descriptor) {
            return Err(failure(
                "downloaded chunk failed length or BLAKE3 validation",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| failure("cache path has no parent"))?;
        fs::create_dir_all(parent)
            .map_err(|error| failure(format!("could not create chunk cache: {error}")))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| failure(format!("could not create cache temporary file: {error}")))?;
        temporary
            .write_all(&bytes)
            .map_err(|error| failure(format!("could not write cache entry: {error}")))?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(failure(format!(
                    "could not publish cache entry: {}",
                    error.error
                )));
            }
        }
        Ok(bytes)
    }
}

impl TransferBackend for HttpBackend {
    fn upload(&self, request: &UploadRequest) -> Result<u64, BackendError> {
        let action = Self::action(request.action.as_ref())?;
        let file = File::open(&request.path)
            .map_err(|error| failure(format!("could not open upload source: {error}")))?;
        let manifest = ChunkStream::new(file, ChunkingProfile::beta_v1())
            .finish()
            .map_err(|error| failure(error.to_string()))?;
        if manifest.object_oid != request.oid || manifest.object_size != request.size {
            return Err(failure(
                "upload source does not match the Git LFS OID and size",
            ));
        }
        let begin = BeginUploadRequest {
            protocol_version: 1,
            manifest,
        };
        let plan: BeginUploadResponse = Self::authorized(self.client.post(&action.href), action)
            .json(&begin)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| failure(format!("could not begin CDC upload: {error}")))?
            .json()
            .map_err(|error| failure(format!("invalid CDC upload plan: {error}")))?;
        let file = File::open(&request.path)
            .map_err(|error| failure(format!("could not reopen upload source: {error}")))?;
        let mut stream = ChunkStream::new(file, ChunkingProfile::beta_v1());
        for (index, chunk) in stream.by_ref().enumerate() {
            let chunk = chunk.map_err(|error| failure(error.to_string()))?;
            let index = u32::try_from(index).map_err(|_| failure("object has too many chunks"))?;
            if plan.missing_chunk_indexes.contains(&index) {
                let url = child_url(
                    &action.href,
                    &[&plan.upload_id.to_string(), "chunks", &index.to_string()],
                )?;
                Self::authorized(self.client.put(url), action)
                    .body(chunk.data)
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .map_err(|error| failure(format!("chunk upload failed: {error}")))?;
            }
        }
        let finalize = child_url(&action.href, &[&plan.upload_id.to_string(), "finalize"])?;
        Self::authorized(self.client.post(finalize), action)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| failure(format!("could not finalize CDC upload: {error}")))?;
        Ok(request.size)
    }

    fn download(&self, request: &DownloadRequest) -> Result<DownloadResult, BackendError> {
        let action = Self::action(request.action.as_ref())?;
        let manifest: ObjectManifest = Self::authorized(self.client.get(&action.href), action)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| failure(format!("could not fetch CDC manifest: {error}")))?
            .json()
            .map_err(|error| failure(format!("invalid CDC manifest: {error}")))?;
        manifest
            .validate()
            .map_err(|error| failure(format!("invalid CDC manifest: {error}")))?;
        if manifest.object_oid != request.oid || manifest.object_size != request.size {
            return Err(failure(
                "server manifest does not match the requested Git LFS object",
            ));
        }
        fs::create_dir_all(&self.cache_root)
            .map_err(|error| failure(format!("could not create cache root: {error}")))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.cache_root)
            .map_err(|error| failure(format!("could not create download file: {error}")))?;
        let mut hasher = Sha256::new();
        for (index, descriptor) in manifest.chunks.iter().enumerate() {
            let bytes = self.read_or_fetch_chunk(action, index, descriptor)?;
            hasher.update(&bytes);
            temporary
                .write_all(&bytes)
                .map_err(|error| failure(format!("could not reconstruct download: {error}")))?;
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if actual.as_slice() != request.oid.as_bytes() {
            return Err(failure("reconstructed download failed SHA-256 validation"));
        }
        let (_file, path) = temporary
            .keep()
            .map_err(|error| failure(format!("could not retain completed download: {error}")))?;
        Ok(DownloadResult {
            path,
            bytes_transferred: request.size,
        })
    }
}

fn valid_chunk(bytes: &[u8], descriptor: &git_cdc_core::ChunkDescriptor) -> bool {
    bytes.len() == descriptor.length as usize
        && blake3::hash(bytes).as_bytes() == descriptor.id.as_bytes()
}

fn child_url(base: &str, children: &[&str]) -> Result<Url, BackendError> {
    let mut url =
        Url::parse(base).map_err(|error| failure(format!("invalid action URL: {error}")))?;
    url.path_segments_mut()
        .map_err(|()| failure("action URL cannot contain path segments"))?
        .extend(children);
    Ok(url)
}

fn failure(message: impl Into<String>) -> BackendError {
    BackendError::new(1, message)
}
