//! Native Git configuration lifecycle using the compiled CLI.
#![allow(
    clippy::unwrap_used,
    reason = "CLI integration fixtures fail immediately"
)]

use std::process::Command;

#[test]
fn install_configure_status_and_uninstall_are_symmetric() {
    let repository = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .arg("init")
            .arg(repository.path())
            .status()
            .unwrap()
            .success()
    );
    let binary = env!("CARGO_BIN_EXE_git-lfs-delta");
    let run = |arguments: &[&str]| {
        Command::new(binary)
            .current_dir(repository.path())
            .args(arguments)
            .output()
            .unwrap()
    };
    assert!(run(&["install", "--scope", "local"]).status.success());
    let configured_path = Command::new("git")
        .current_dir(repository.path())
        .args(["config", "--get", "lfs.customtransfer.cdc.path"])
        .output()
        .unwrap();
    assert!(configured_path.status.success());
    let configured_path = String::from_utf8(configured_path.stdout).unwrap();
    assert!(!configured_path.trim().is_empty());
    #[cfg(windows)]
    assert!(
        !configured_path.trim().starts_with(r"\\?\"),
        "Git LFS cannot reliably launch a verbatim Windows path: {configured_path}"
    );
    assert!(
        run(&[
            "configure",
            "--scope",
            "local",
            "--url",
            "https://forge.example/team/assets/info/lfs"
        ])
        .status
        .success()
    );
    let status = String::from_utf8(run(&["status"]).stdout).unwrap();
    assert!(status.contains("lfs.customtransfer.cdc.concurrent=true"));
    assert!(status.contains("lfs.url=https://forge.example/team/assets/info/lfs"));
    assert!(run(&["uninstall", "--scope", "local"]).status.success());
    let status = String::from_utf8(run(&["status"]).stdout).unwrap();
    assert!(status.contains("lfs.customtransfer.cdc.path=<unset>"));
}
