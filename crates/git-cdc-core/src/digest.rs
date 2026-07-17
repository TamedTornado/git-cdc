use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

const DIGEST_BYTES: usize = 32;
const DIGEST_HEX_LENGTH: usize = DIGEST_BYTES * 2;

macro_rules! digest_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; DIGEST_BYTES]);

        impl $name {
            pub(crate) const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
                Self(bytes)
            }

            /// Returns the raw 256-bit digest.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, formatter)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&hex::encode(self.0))
            }
        }

        impl FromStr for $name {
            type Err = DigestParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.len() != DIGEST_HEX_LENGTH {
                    return Err(DigestParseError::Length(value.len()));
                }
                let mut bytes = [0_u8; DIGEST_BYTES];
                hex::decode_to_slice(value, &mut bytes).map_err(DigestParseError::Hex)?;
                Ok(Self(bytes))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }
    };
}

digest_type!(ChunkId, "A BLAKE3 identity for an immutable chunk.");
digest_type!(
    ObjectOid,
    "The canonical SHA-256 identity from a Git LFS pointer."
);

/// Failure while parsing a 256-bit hexadecimal digest.
#[derive(Debug, thiserror::Error)]
pub enum DigestParseError {
    /// The input did not contain exactly 64 hexadecimal characters.
    #[error("digest must contain 64 hexadecimal characters, received {0}")]
    Length(usize),
    /// The input contained a non-hexadecimal character.
    #[error("digest is not valid hexadecimal: {0}")]
    Hex(#[from] hex::FromHexError),
}
