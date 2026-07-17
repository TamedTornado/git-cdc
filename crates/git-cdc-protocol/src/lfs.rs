use std::collections::BTreeMap;

use git_cdc_core::ObjectOid;
use serde::{Deserialize, Serialize};
use url::Url;

/// Operation requested through the Git LFS Batch API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Upload one or more logical objects.
    Upload,
    /// Download one or more logical objects.
    Download,
}

/// Transfer mechanism selected for a Batch API response.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferKind {
    /// Standard whole-object HTTP transfer.
    Basic,
    /// Git-CDC chunk-aware custom transfer.
    Cdc,
}

impl TransferKind {
    /// Returns the Git LFS wire name for this transfer mechanism.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Cdc => "cdc",
        }
    }
}

/// Selects the best mutually supported transfer mechanism.
///
/// An absent list means `basic`, as required for older Git LFS clients.
///
/// # Errors
///
/// Returns [`TransferSelectionError`] if a non-empty list contains no transfer
/// mechanism supported by Git-CDC.
pub fn select_transfer(offered: &[String]) -> Result<TransferKind, TransferSelectionError> {
    if offered.is_empty() {
        return Ok(TransferKind::Basic);
    }
    if offered.iter().any(|transfer| transfer == "cdc") {
        return Ok(TransferKind::Cdc);
    }
    if offered.iter().any(|transfer| transfer == "basic") {
        return Ok(TransferKind::Basic);
    }
    Err(TransferSelectionError)
}

/// No client-offered Git LFS transfer mechanism was supported.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("the client offered no supported Git LFS transfer mechanism")]
pub struct TransferSelectionError;

/// Optional Git reference associated with a Batch request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LfsReference {
    /// Fully qualified Git reference name.
    pub name: String,
}

/// One canonical logical object requested through Git LFS.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchObject {
    /// Canonical SHA-256 Git LFS object identity.
    pub oid: ObjectOid,
    /// Logical object size in bytes.
    pub size: u64,
}

/// Standard Git LFS Batch API request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchRequest {
    /// Requested upload or download operation.
    pub operation: Operation,
    /// Client-supported transfer mechanisms, in client preference order.
    #[serde(default)]
    pub transfers: Vec<String>,
    /// Optional Git reference associated with the operation.
    #[serde(default, rename = "ref")]
    pub reference: Option<LfsReference>,
    /// Logical objects included in the batch.
    pub objects: Vec<BatchObject>,
}

/// One URL action in a Git LFS Batch response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LfsAction {
    /// Authorized URL for the action.
    pub href: Url,
    /// Headers the client or transfer agent must attach.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub header: BTreeMap<String, String>,
    /// Optional RFC 3339 authorization expiry timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Per-object error returned by the Batch API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchObjectError {
    /// HTTP-compatible error status.
    pub code: u16,
    /// Human-readable error message.
    pub message: String,
}

/// Per-object result returned by the Batch API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchObjectResponse {
    /// Canonical SHA-256 Git LFS object identity.
    pub oid: ObjectOid,
    /// Logical object size in bytes.
    pub size: u64,
    /// Actions keyed by `upload`, `download`, or `verify`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub actions: BTreeMap<String, LfsAction>,
    /// Object-specific failure when no action can be offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BatchObjectError>,
}

/// Standard Git LFS Batch API response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BatchResponse {
    /// Transfer mechanism selected for the batch.
    pub transfer: TransferKind,
    /// Results in the same order as the request objects.
    pub objects: Vec<BatchObjectResponse>,
}

/// Owner details attached to an LFS lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockOwner {
    /// Stable user or service identity.
    pub name: String,
}

/// Standard Git LFS lock representation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Lock {
    /// Server-generated lock identifier.
    pub id: String,
    /// Repository-relative locked path.
    pub path: String,
    /// RFC 3339 creation timestamp.
    pub locked_at: String,
    /// Identity that owns the lock.
    pub owner: LockOwner,
}

/// Request to lock one repository-relative path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockRequest {
    /// Repository-relative path to lock.
    pub path: String,
}

/// Response after creating an LFS lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockResponse {
    /// Lock that was created.
    pub lock: Lock,
}

/// Paginated lock listing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockList {
    /// Locks visible to the caller.
    pub locks: Vec<Lock>,
    /// Opaque cursor for the next page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Standard Git LFS lock-verification response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockVerifyResponse {
    /// Locks owned by the authenticated identity.
    pub ours: Vec<Lock>,
    /// Locks owned by other identities.
    pub theirs: Vec<Lock>,
    /// Opaque cursor for the next page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Request to release an LFS lock.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnlockRequest {
    /// Allow an authorized caller to release another owner's lock.
    #[serde(default)]
    pub force: bool,
}

/// Response after releasing an LFS lock.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnlockResponse {
    /// Lock that was released.
    pub lock: Lock,
}
