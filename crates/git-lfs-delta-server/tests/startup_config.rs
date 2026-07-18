//! Fail-closed process startup contracts.
#![allow(clippy::unwrap_used, reason = "process fixtures fail immediately")]

use std::process::Command;

#[test]
fn authentication_mode_is_mandatory_before_external_services_are_contacted() {
    let output = Command::new(env!("CARGO_BIN_EXE_git-lfs-delta-server"))
        .env_remove("GIT_LFS_DELTA_AUTH_MODE")
        .env(
            "GIT_LFS_DELTA_DATABASE_URL",
            "postgres://invalid.invalid/git_lfs_delta",
        )
        .env("GIT_LFS_DELTA_BASE_URL", "http://127.0.0.1:8080/")
        .env(
            "GIT_LFS_DELTA_STORAGE_URL",
            "file:///tmp/git-lfs-delta-test",
        )
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("GIT_LFS_DELTA_AUTH_MODE")
    );
}

#[test]
fn development_authentication_cannot_accidentally_bind_publicly() {
    let output = Command::new(env!("CARGO_BIN_EXE_git-lfs-delta-server"))
        .env("GIT_LFS_DELTA_AUTH_MODE", "development")
        .env("GIT_LFS_DELTA_DEV_TOKEN", "test-only")
        .env("GIT_LFS_DELTA_BIND", "0.0.0.0:8080")
        .env(
            "GIT_LFS_DELTA_DATABASE_URL",
            "postgres://invalid.invalid/git_lfs_delta",
        )
        .env("GIT_LFS_DELTA_BASE_URL", "http://127.0.0.1:8080/")
        .env(
            "GIT_LFS_DELTA_STORAGE_URL",
            "file:///tmp/git-lfs-delta-test",
        )
        .env_remove("GIT_LFS_DELTA_ALLOW_REMOTE_DEVELOPMENT_AUTH")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("development authentication may not bind remotely")
    );
}
