//! Fail-closed process startup contracts.
#![allow(clippy::unwrap_used, reason = "process fixtures fail immediately")]

use std::process::Command;

#[test]
fn authentication_mode_is_mandatory_before_external_services_are_contacted() {
    let output = Command::new(env!("CARGO_BIN_EXE_git-cdc-server"))
        .env_remove("GIT_CDC_AUTH_MODE")
        .env("GIT_CDC_DATABASE_URL", "postgres://invalid.invalid/git_cdc")
        .env("GIT_CDC_BASE_URL", "http://127.0.0.1:8080/")
        .env("GIT_CDC_STORAGE_URL", "file:///tmp/git-cdc-test")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("GIT_CDC_AUTH_MODE")
    );
}
