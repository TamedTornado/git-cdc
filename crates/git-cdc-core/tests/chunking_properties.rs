//! Property tests for chunk reconstruction and manifest invariants.
#![allow(
    clippy::unwrap_used,
    reason = "a generated failing case must abort and be persisted by proptest"
)]

use std::io::Cursor;

use git_cdc_core::{ChunkStream, ChunkingProfile};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn arbitrary_inputs_reconstruct_exactly(source in proptest::collection::vec(any::<u8>(), 0..2_000_000)) {
        let mut stream = ChunkStream::new(Cursor::new(source.clone()), ChunkingProfile::beta_v1());
        let mut reconstructed = Vec::with_capacity(source.len());

        for chunk in stream.by_ref() {
            reconstructed.extend_from_slice(&chunk.unwrap().data);
        }
        let manifest = stream.finish().unwrap();

        prop_assert_eq!(&reconstructed, &source);
        prop_assert_eq!(manifest.object_size, source.len() as u64);
        prop_assert_eq!(
            manifest.chunks.iter().map(|chunk| u64::from(chunk.length)).sum::<u64>(),
            manifest.object_size
        );
    }
}
