//! Real Git/Git-LFS reachability scan contract.
#![allow(clippy::unwrap_used, reason = "integration fixtures fail immediately")]

use std::{fs, process::Command};

use git_cdc_server::reconcile::scan;

fn git(directory: &std::path::Path, arguments: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn scan_finds_lfs_pointers_reachable_from_branches_and_tags() {
    let repository = tempfile::tempdir().unwrap();
    git(repository.path(), &["init", "-b", "master"]);
    git(repository.path(), &["config", "user.name", "Git CDC Test"]);
    git(
        repository.path(),
        &["config", "user.email", "test@git-cdc.invalid"],
    );
    fs::write(
        repository.path().join(".gitattributes"),
        "*.bin filter=lfs diff=lfs merge=lfs -text\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("asset.bin"),
        "version https://git-lfs.github.com/spec/v1\noid sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\nsize 0\n",
    ).unwrap();
    git(repository.path(), &["add", ".gitattributes", "asset.bin"]);
    git(repository.path(), &["commit", "-m", "fixture"]);
    git(repository.path(), &["tag", "v1"]);

    let result = scan(repository.path().to_str().unwrap()).unwrap();

    assert_eq!(result.ref_fingerprint.len(), 64);
    assert_eq!(result.reachable_objects.len(), 1);
    assert_eq!(
        result.reachable_objects[0].to_string(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
