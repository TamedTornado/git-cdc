//! Bounded-memory HTTP implementation of the CDC transfer data plane.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Write,
    path::PathBuf,
    thread,
    time::Duration,
};

use git_lfs_delta_core::{ChunkStream, ChunkingProfile, ObjectManifest};
use git_lfs_delta_protocol::{BeginUploadRequest, BeginUploadResponse};
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
    chunk_concurrency: usize,
}

impl HttpBackend {
    /// Creates a backend with an explicit cross-platform cache directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(cache_root: PathBuf) -> Result<Self, BackendError> {
        let connect_timeout = environment_u64("GIT_LFS_DELTA_HTTP_CONNECT_TIMEOUT_SECONDS", 10)?;
        let request_timeout = environment_u64("GIT_LFS_DELTA_HTTP_REQUEST_TIMEOUT_SECONDS", 300)?;
        let chunk_concurrency =
            environment_usize("GIT_LFS_DELTA_CHUNK_CONCURRENCY", 2)?.clamp(1, 8);
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(connect_timeout))
            .timeout(Duration::from_secs(request_timeout))
            .build()
            .map_err(|error| failure(format!("could not create HTTP client: {error}")))?;
        Ok(Self {
            client,
            cache_root,
            chunk_concurrency,
        })
    }

    fn action(action: Option<&TransferAction>) -> Result<&TransferAction, BackendError> {
        action.ok_or_else(|| failure("server did not provide a CDC transfer action"))
    }

    fn begin_upload(
        &self,
        action: &TransferAction,
        begin: &BeginUploadRequest,
    ) -> Result<BeginUploadResponse, BackendError> {
        for attempt in 0..3_u32 {
            match Self::authorized(self.client.post(&action.href), action)
                .json(begin)
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    return response
                        .json()
                        .map_err(|error| failure(format!("invalid CDC upload plan: {error}")));
                }
                Ok(response) if retryable_status(response.status()) && attempt < 2 => {
                    thread::sleep(
                        retry_after(&response)
                            .unwrap_or_else(|| Duration::from_millis(100 * 2_u64.pow(attempt))),
                    );
                }
                Ok(response) => {
                    return Err(failure(format!(
                        "could not begin CDC upload: HTTP {}",
                        response.status()
                    )));
                }
                Err(_) if attempt < 2 => {
                    thread::sleep(Duration::from_millis(100 * 2_u64.pow(attempt)));
                }
                Err(error) => {
                    return Err(failure(format!("could not begin CDC upload: {error}")));
                }
            }
        }
        Err(failure("could not begin CDC upload after retries"))
    }

    fn finalize_upload(
        &self,
        action: &TransferAction,
        upload_id: uuid::Uuid,
    ) -> Result<(), BackendError> {
        let url = child_url(&action.href, &[&upload_id.to_string(), "finalize"])?;
        for attempt in 0..3_u32 {
            match Self::authorized(self.client.post(url.clone()), action).send() {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) if retryable_status(response.status()) && attempt < 2 => {
                    thread::sleep(
                        retry_after(&response)
                            .unwrap_or_else(|| Duration::from_millis(100 * 2_u64.pow(attempt))),
                    );
                }
                Ok(response) => {
                    return Err(failure(format!(
                        "could not finalize CDC upload: HTTP {}",
                        response.status()
                    )));
                }
                Err(_) if attempt < 2 => {
                    thread::sleep(Duration::from_millis(100 * 2_u64.pow(attempt)));
                }
                Err(error) => {
                    return Err(failure(format!("could not finalize CDC upload: {error}")));
                }
            }
        }
        Err(failure("could not finalize CDC upload after retries"))
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
        descriptor: &git_lfs_delta_core::ChunkDescriptor,
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
        let mut downloaded = None;
        let mut last_error = None;
        for attempt in 0..3_u32 {
            let mut delay = Duration::from_millis(100 * 2_u64.pow(attempt));
            match Self::authorized(self.client.get(url.clone()), action).send() {
                Ok(response) if response.status().is_success() => {
                    downloaded = Some(
                        response
                            .bytes()
                            .map_err(|error| {
                                failure(format!("could not read chunk response: {error}"))
                            })?
                            .to_vec(),
                    );
                    break;
                }
                Ok(response) if retryable_status(response.status()) => {
                    delay = retry_after(&response).unwrap_or(delay);
                    last_error = Some(format!("HTTP {}", response.status()));
                }
                Ok(response) => {
                    return Err(failure(format!(
                        "chunk download failed with HTTP {}",
                        response.status()
                    )));
                }
                Err(error) => last_error = Some(error.to_string()),
            }
            if attempt < 2 {
                thread::sleep(delay);
            }
        }
        let bytes = downloaded.ok_or_else(|| {
            failure(format!(
                "chunk download failed after retries: {}",
                last_error.unwrap_or_else(|| "unknown transport failure".into())
            ))
        })?;
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

    fn upload_chunk_with_retry(
        &self,
        action: &TransferAction,
        upload_id: uuid::Uuid,
        index: u32,
        bytes: &[u8],
    ) -> Result<(), BackendError> {
        let url = child_url(
            &action.href,
            &[&upload_id.to_string(), "chunks", &index.to_string()],
        )?;
        let mut last_error = None;
        for attempt in 0..3_u32 {
            let mut delay = Duration::from_millis(100 * 2_u64.pow(attempt));
            match Self::authorized(self.client.put(url.clone()), action)
                .body(bytes.to_vec())
                .send()
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) if retryable_status(response.status()) => {
                    delay = retry_after(&response).unwrap_or(delay);
                    last_error = Some(format!("HTTP {}", response.status()));
                }
                Ok(response) => {
                    return Err(failure(format!(
                        "chunk upload failed with HTTP {}",
                        response.status()
                    )));
                }
                Err(error) => last_error = Some(error.to_string()),
            }
            if attempt < 2 {
                thread::sleep(delay);
            }
        }
        Err(failure(format!(
            "chunk upload failed after retries: {}",
            last_error.unwrap_or_else(|| "unknown transport failure".into())
        )))
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
        let plan = self.begin_upload(action, &begin)?;
        if plan.protocol_version != 1 {
            return Err(failure(
                "server selected an unsupported CDC protocol version",
            ));
        }
        if plan
            .missing_chunk_indexes
            .iter()
            .any(|index| *index as usize >= begin.manifest.chunks.len())
        {
            return Err(failure(
                "server upload plan contains a chunk index outside the manifest",
            ));
        }
        let file = File::open(&request.path)
            .map_err(|error| failure(format!("could not reopen upload source: {error}")))?;
        let missing: HashSet<u32> = plan.missing_chunk_indexes.iter().copied().collect();
        if missing.len() != plan.missing_chunk_indexes.len() {
            return Err(failure(
                "server upload plan contains duplicate chunk indexes",
            ));
        }
        let (jobs, work) = crossbeam_channel::bounded::<(u32, Vec<u8>)>(self.chunk_concurrency);
        let (failures, errors) = crossbeam_channel::unbounded();
        let mut producer_error = None;
        thread::scope(|scope| {
            for _ in 0..self.chunk_concurrency {
                let work = work.clone();
                let failures = failures.clone();
                scope.spawn(move || {
                    for (index, data) in work {
                        if let Err(error) =
                            self.upload_chunk_with_retry(action, plan.upload_id, index, &data)
                        {
                            let _ = failures.send(error);
                            break;
                        }
                    }
                });
            }
            drop(work);
            drop(failures);
            let mut stream = ChunkStream::new(file, ChunkingProfile::beta_v1());
            for (index, chunk) in stream.by_ref().enumerate() {
                let result = chunk
                    .map_err(|error| failure(error.to_string()))
                    .and_then(|chunk| {
                        let index = u32::try_from(index)
                            .map_err(|_| failure("object has too many chunks"))?;
                        if missing.contains(&index) {
                            jobs.send((index, chunk.data))
                                .map_err(|_| failure("chunk upload worker stopped"))?;
                        }
                        Ok(())
                    });
                if let Err(error) = result {
                    producer_error = Some(error);
                    break;
                }
                if let Ok(error) = errors.try_recv() {
                    producer_error = Some(error);
                    break;
                }
            }
            drop(jobs);
        });
        if let Some(error) = producer_error.or_else(|| errors.try_recv().ok()) {
            return Err(error);
        }
        self.finalize_upload(action, plan.upload_id)?;
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
        let (jobs, work) = crossbeam_channel::bounded(self.chunk_concurrency);
        let (completed, results) = crossbeam_channel::bounded(self.chunk_concurrency);
        thread::scope(|scope| -> Result<(), BackendError> {
            for _ in 0..self.chunk_concurrency {
                let work = work.clone();
                let completed = completed.clone();
                scope.spawn(move || {
                    for (index, descriptor) in work {
                        let result = self.read_or_fetch_chunk(action, index, &descriptor);
                        if completed.send((index, result)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(work);
            drop(completed);
            let total = manifest.chunks.len();
            let mut next_scheduled = 0_usize;
            while next_scheduled < total && next_scheduled < self.chunk_concurrency {
                jobs.send((next_scheduled, manifest.chunks[next_scheduled].clone()))
                    .map_err(|_| failure("chunk download worker stopped"))?;
                next_scheduled += 1;
            }
            let mut next_write = 0_usize;
            let mut pending = BTreeMap::new();
            while next_write < total {
                let (index, result) = results
                    .recv()
                    .map_err(|_| failure("chunk download worker stopped"))?;
                pending.insert(index, result?);
                while let Some(bytes) = pending.remove(&next_write) {
                    hasher.update(&bytes);
                    temporary.write_all(&bytes).map_err(|error| {
                        failure(format!("could not reconstruct download: {error}"))
                    })?;
                    next_write += 1;
                    if next_scheduled < total {
                        jobs.send((next_scheduled, manifest.chunks[next_scheduled].clone()))
                            .map_err(|_| failure("chunk download worker stopped"))?;
                        next_scheduled += 1;
                    }
                }
            }
            drop(jobs);
            Ok(())
        })?;
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

fn valid_chunk(bytes: &[u8], descriptor: &git_lfs_delta_core::ChunkDescriptor) -> bool {
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

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn retry_after(response: &reqwest::blocking::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn environment_u64(name: &str, default: u64) -> Result<u64, BackendError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| failure(format!("invalid {name}: {error}"))),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(failure(format!("could not read {name}: {error}"))),
    }
}

fn environment_usize(name: &str, default: usize) -> Result<usize, BackendError> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| failure(format!("invalid {name}: {error}"))),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(failure(format!("could not read {name}: {error}"))),
    }
}
