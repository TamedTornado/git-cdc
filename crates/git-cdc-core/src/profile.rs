use serde::{Deserialize, Serialize};

/// A versioned, deterministic content-defined chunking profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ChunkingProfile {
    /// `FastCDC` 2020, normalization level 1, 512 KiB/2 MiB/8 MiB.
    #[serde(rename = "fastcdc-v1")]
    FastCdcV1,
}

impl ChunkingProfile {
    /// Returns the mandatory profile for the first Git-CDC protocol version.
    #[must_use]
    pub const fn beta_v1() -> Self {
        Self::FastCdcV1
    }

    /// Returns the minimum, target, and maximum chunk sizes for this profile.
    #[must_use]
    pub const fn sizes(self) -> (usize, usize, usize) {
        match self {
            Self::FastCdcV1 => (512 * 1024, 2 * 1024 * 1024, 8 * 1024 * 1024),
        }
    }
}
