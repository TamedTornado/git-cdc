//! Cross-platform contracts for deterministic object manifests.
#![allow(
    clippy::unwrap_used,
    reason = "contract tests should fail immediately when fixture construction fails"
)]

use std::io::Cursor;

use git_cdc_core::{ChunkStream, ChunkingProfile, MAX_OBJECT_SIZE, ManifestError, ManifestVersion};

fn patterned_bytes(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| {
            let mixed = index.wrapping_mul(31) ^ index.rotate_left(7) ^ (index / 251);
            mixed.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn empty_input_has_canonical_sha256_and_no_chunks() {
    let stream = ChunkStream::new(Cursor::new(Vec::<u8>::new()), ChunkingProfile::beta_v1());
    let manifest = stream.finish().unwrap();

    assert_eq!(manifest.version, ManifestVersion::V1);
    assert_eq!(manifest.object_size, 0);
    assert_eq!(
        manifest.object_oid.to_string(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert!(manifest.chunks.is_empty());
}

#[test]
fn streamed_chunks_reconstruct_the_source_and_manifest_offsets_are_contiguous() {
    let source = patterned_bytes(18 * 1024 * 1024 + 173);
    let mut stream = ChunkStream::new(Cursor::new(source.clone()), ChunkingProfile::beta_v1());
    let mut reconstructed = Vec::new();

    for chunk in stream.by_ref() {
        let chunk = chunk.unwrap();
        reconstructed.extend_from_slice(&chunk.data);
    }
    let manifest = stream.finish().unwrap();

    assert_eq!(reconstructed, source);
    assert_eq!(manifest.object_size, source.len() as u64);
    assert!(manifest.chunks.len() >= 3);

    let mut expected_offset = 0_u64;
    for descriptor in &manifest.chunks {
        assert_eq!(descriptor.offset, expected_offset);
        assert!(descriptor.length > 0);
        expected_offset += u64::from(descriptor.length);
    }
    assert_eq!(expected_offset, manifest.object_size);
}

#[test]
fn a_local_edit_preserves_most_chunk_identities() {
    let original = patterned_bytes(32 * 1024 * 1024);
    let mut edited = original.clone();
    edited[15 * 1024 * 1024..15 * 1024 * 1024 + 128].fill(0xA5);

    let original_manifest = ChunkStream::new(Cursor::new(original), ChunkingProfile::beta_v1())
        .collect_manifest()
        .unwrap();
    let edited_manifest = ChunkStream::new(Cursor::new(edited), ChunkingProfile::beta_v1())
        .collect_manifest()
        .unwrap();

    let preserved = original_manifest
        .chunks
        .iter()
        .filter(|chunk| {
            edited_manifest
                .chunks
                .iter()
                .any(|candidate| candidate.id == chunk.id)
        })
        .count();

    assert!(preserved >= original_manifest.chunks.len().saturating_sub(2));
}

#[test]
fn manifests_have_a_stable_json_representation() {
    let manifest = ChunkStream::new(
        Cursor::new(patterned_bytes(5 * 1024 * 1024)),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();

    let json = serde_json::to_string(&manifest).unwrap();
    let decoded = serde_json::from_str(&json).unwrap();

    assert_eq!(manifest, decoded);
    assert!(json.contains("\"version\":\"v1\""));
    assert!(json.contains("\"profile\":\"fastcdc-v1\""));
}

#[test]
fn beta_v1_has_a_cross_platform_golden_manifest() {
    let manifest = ChunkStream::new(
        Cursor::new(patterned_bytes(12 * 1024 * 1024 + 37)),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();

    assert_eq!(
        serde_json::to_string(&manifest).unwrap(),
        "{\"version\":\"v1\",\"profile\":\"fastcdc-v1\",\"object_oid\":\"36e0d422a31ba1fc4f852eaca7b55e43ac0804155a0237baacb5718294c8f83c\",\"object_size\":12582949,\"chunks\":[{\"id\":\"0dae2c7cdfa40f65daa6a36e8413c722c259db2dd01ba0dc411fff7486f5f9ba\",\"offset\":0,\"length\":2112183},{\"id\":\"cc9a1609d366cf126f13701ce19cb8820cc8d688837ee1ea9b4dfbcc724e711d\",\"offset\":2112183,\"length\":2120448},{\"id\":\"cc9a1609d366cf126f13701ce19cb8820cc8d688837ee1ea9b4dfbcc724e711d\",\"offset\":4232631,\"length\":2120448},{\"id\":\"cc9a1609d366cf126f13701ce19cb8820cc8d688837ee1ea9b4dfbcc724e711d\",\"offset\":6353079,\"length\":2120448},{\"id\":\"cc9a1609d366cf126f13701ce19cb8820cc8d688837ee1ea9b4dfbcc724e711d\",\"offset\":8473527,\"length\":2120448},{\"id\":\"c271f0ba6fc132ac2f8be326aa36082b35d199eea05890eed2796688e4498589\",\"offset\":10593975,\"length\":1988974}]}"
    );
}

#[test]
fn rejects_non_contiguous_manifest_offsets() {
    let mut manifest = ChunkStream::new(
        Cursor::new(patterned_bytes(2 * 1024 * 1024)),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();
    manifest.chunks[0].offset = 1;

    assert!(manifest.validate().is_err());
}

#[test]
fn rejects_manifest_size_that_disagrees_with_chunks() {
    let mut manifest = ChunkStream::new(
        Cursor::new(patterned_bytes(2 * 1024 * 1024)),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();
    manifest.object_size += 1;

    assert!(manifest.validate().is_err());
}

#[test]
fn accepts_the_canonical_empty_object_manifest() {
    let manifest = ChunkStream::new(Cursor::new(Vec::<u8>::new()), ChunkingProfile::beta_v1())
        .collect_manifest()
        .unwrap();

    assert_eq!(manifest.validate(), Ok(()));
}

#[test]
fn rejects_resource_amplifying_manifests() {
    let mut too_large = ChunkStream::new(Cursor::new(Vec::<u8>::new()), ChunkingProfile::beta_v1())
        .collect_manifest()
        .unwrap();
    too_large.object_size = MAX_OBJECT_SIZE + 1;
    assert!(matches!(
        too_large.validate(),
        Err(ManifestError::ObjectTooLarge { .. })
    ));

    let mut undersized = ChunkStream::new(
        Cursor::new(patterned_bytes(12 * 1024 * 1024)),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();
    undersized.chunks[0].length = 1;
    assert!(matches!(
        undersized.validate(),
        Err(ManifestError::ChunkTooSmall { .. })
    ));

    let mut oversized = ChunkStream::new(
        Cursor::new(patterned_bytes(2 * 1024 * 1024)),
        ChunkingProfile::beta_v1(),
    )
    .collect_manifest()
    .unwrap();
    oversized.chunks[0].length = 8 * 1024 * 1024 + 1;
    assert!(matches!(
        oversized.validate(),
        Err(ManifestError::ChunkTooLarge { .. })
    ));
}
