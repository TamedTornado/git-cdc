use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
};

use git_lfs_delta_core::ObjectOid;
use serde::{Deserialize, Serialize};

/// Action data copied from the server's Git LFS Batch response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TransferAction {
    /// Primary endpoint or mechanism-specific location.
    pub href: String,
    /// Headers supplied by the server for the authorized transfer.
    #[serde(default)]
    pub header: BTreeMap<String, String>,
}

/// One Git LFS upload request passed to the custom-transfer agent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct UploadRequest {
    /// Canonical Git LFS SHA-256 identity.
    pub oid: ObjectOid,
    /// Expected logical file size.
    pub size: u64,
    /// File Git LFS asks the agent to upload.
    pub path: PathBuf,
    /// Server-selected transfer action, absent for standalone agents.
    pub action: Option<TransferAction>,
}

/// One Git LFS download request passed to the custom-transfer agent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct DownloadRequest {
    /// Canonical Git LFS SHA-256 identity.
    pub oid: ObjectOid,
    /// Expected logical file size.
    pub size: u64,
    /// Server-selected transfer action, absent for standalone agents.
    pub action: Option<TransferAction>,
}

/// Successful result returned by a download backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadResult {
    /// Temporary file relinquished to Git LFS after completion.
    pub path: PathBuf,
    /// Logical bytes successfully transferred.
    pub bytes_transferred: u64,
}

/// A transfer-specific failure that must be reported without killing the agent.
#[derive(Clone, Debug, thiserror::Error)]
#[error("transfer failed ({code}): {message}")]
pub struct BackendError {
    /// Process-independent error code reported to Git LFS.
    pub code: i32,
    /// Human-readable failure message.
    pub message: String,
}

impl BackendError {
    /// Creates a transfer failure with a stable code and message.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Data-plane implementation used by the Git LFS protocol driver.
pub trait TransferBackend {
    /// Uploads one logical object and returns its transferred byte count.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] for an object-specific failure.
    fn upload(&self, request: &UploadRequest) -> Result<u64, BackendError>;

    /// Downloads one logical object into a temporary file.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] for an object-specific failure.
    fn download(&self, request: &DownloadRequest) -> Result<DownloadResult, BackendError>;
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum InputMessage {
    Init {
        #[serde(rename = "operation")]
        _operation: String,
        #[serde(rename = "remote")]
        _remote: String,
        #[serde(rename = "concurrent")]
        _concurrent: bool,
        #[serde(rename = "concurrenttransfers")]
        _concurrent_transfers: usize,
    },
    Upload {
        #[serde(flatten)]
        request: UploadRequest,
    },
    Download {
        #[serde(flatten)]
        request: DownloadRequest,
    },
    Terminate,
}

#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
enum OutputMessage<'a> {
    Progress {
        oid: ObjectOid,
        #[serde(rename = "bytesSoFar")]
        bytes_so_far: u64,
        #[serde(rename = "bytesSinceLast")]
        bytes_since_last: u64,
    },
    Complete {
        oid: ObjectOid,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WireError<'a>>,
    },
}

#[derive(Debug, Serialize)]
struct WireError<'a> {
    code: i32,
    message: &'a str,
}

/// Fatal framing or state failure in the custom-transfer process.
#[derive(Debug, thiserror::Error)]
pub enum TransferProtocolError {
    /// Reading from Git LFS failed.
    #[error("could not read Git LFS transfer input: {0}")]
    Read(#[source] std::io::Error),
    /// A line was not a valid protocol message.
    #[error("invalid Git LFS transfer message: {0}")]
    Decode(#[source] serde_json::Error),
    /// Writing or flushing a response failed.
    #[error("could not write Git LFS transfer output: {0}")]
    Write(#[source] std::io::Error),
    /// A transfer arrived before the mandatory initialization exchange.
    #[error("Git LFS sent a transfer before initialization")]
    NotInitialized,
    /// Git LFS sent more than one initialization exchange.
    #[error("Git LFS sent duplicate initialization")]
    AlreadyInitialized,
}

/// Runs the complete line-delimited Git LFS custom-transfer protocol.
///
/// Object failures are emitted as completion messages and do not terminate the
/// process. Framing, I/O, and protocol-state failures are fatal.
///
/// # Errors
///
/// Returns [`TransferProtocolError`] for fatal protocol or stream failures.
pub fn run_transfer_protocol<R, W, B>(
    input: R,
    mut output: W,
    backend: &B,
) -> Result<(), TransferProtocolError>
where
    R: Read,
    W: Write,
    B: TransferBackend,
{
    let mut initialized = false;
    for line in BufReader::new(input).lines() {
        let line = line.map_err(TransferProtocolError::Read)?;
        let message = serde_json::from_str(&line).map_err(TransferProtocolError::Decode)?;
        match message {
            InputMessage::Init { .. } => {
                if initialized {
                    return Err(TransferProtocolError::AlreadyInitialized);
                }
                initialized = true;
                write_json_line(&mut output, &serde_json::json!({}))?;
            }
            InputMessage::Upload { request } => {
                require_initialized(initialized)?;
                match backend.upload(&request) {
                    Ok(bytes) => {
                        emit_progress(&mut output, request.oid, bytes)?;
                        emit_complete(&mut output, request.oid, None, None)?;
                    }
                    Err(error) => emit_complete(&mut output, request.oid, None, Some(&error))?,
                }
            }
            InputMessage::Download { request } => {
                require_initialized(initialized)?;
                match backend.download(&request) {
                    Ok(result) => {
                        emit_progress(&mut output, request.oid, result.bytes_transferred)?;
                        let path = result.path.to_string_lossy();
                        emit_complete(&mut output, request.oid, Some(path.as_ref()), None)?;
                    }
                    Err(error) => emit_complete(&mut output, request.oid, None, Some(&error))?,
                }
            }
            InputMessage::Terminate => return Ok(()),
        }
    }
    Ok(())
}

fn require_initialized(initialized: bool) -> Result<(), TransferProtocolError> {
    if initialized {
        Ok(())
    } else {
        Err(TransferProtocolError::NotInitialized)
    }
}

fn emit_progress(
    output: &mut impl Write,
    oid: ObjectOid,
    bytes: u64,
) -> Result<(), TransferProtocolError> {
    write_json_line(
        output,
        &OutputMessage::Progress {
            oid,
            bytes_so_far: bytes,
            bytes_since_last: bytes,
        },
    )
}

fn emit_complete(
    output: &mut impl Write,
    oid: ObjectOid,
    path: Option<&str>,
    error: Option<&BackendError>,
) -> Result<(), TransferProtocolError> {
    write_json_line(
        output,
        &OutputMessage::Complete {
            oid,
            path,
            error: error.map(|error| WireError {
                code: error.code,
                message: &error.message,
            }),
        },
    )
}

fn write_json_line(
    output: &mut impl Write,
    value: &impl Serialize,
) -> Result<(), TransferProtocolError> {
    serde_json::to_writer(&mut *output, value)
        .map_err(|error| TransferProtocolError::Write(std::io::Error::other(error.to_string())))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(TransferProtocolError::Write)
}
