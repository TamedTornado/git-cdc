//! Cross-platform Git LFS custom-transfer agent and configuration helpers.

mod backend;
mod transfer;

pub use backend::HttpBackend;

pub use transfer::{
    BackendError, DownloadRequest, DownloadResult, TransferAction, TransferBackend,
    TransferProtocolError, UploadRequest, run_transfer_protocol,
};
