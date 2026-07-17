//! Forge-neutral read-only Git reachability scanning.

use std::{collections::BTreeSet, process::Command};

use git_cdc_core::ObjectOid;
use sha2::{Digest, Sha256};

/// Complete result of scanning all refs in a freshly fetched mirror.
pub struct ReachabilityScan {
    /// SHA-256 fingerprint of sorted `show-ref` output.
    pub ref_fingerprint: String,
    /// Unique LFS object identities reachable from any mirrored ref.
    pub reachable_objects: Vec<ObjectOid>,
}

/// Clones a read-only mirror and enumerates LFS pointers reachable from all refs.
///
/// No snapshot should be submitted if this function fails.
///
/// # Errors
///
/// Returns [`ScanError`] for Git, Git LFS, output, or pointer failures.
pub fn scan(remote: &str) -> Result<ReachabilityScan, ScanError> {
    let directory = tempfile::tempdir()?;
    let mirror = directory.path().join("repository.git");
    run(Command::new("git")
        .args(["clone", "--mirror", remote])
        .arg(&mirror))?;
    let refs = output(Command::new("git").arg("-C").arg(&mirror).arg("show-ref"))?;
    let ref_fingerprint = hex::encode(Sha256::digest(&refs));
    let refs_text = std::str::from_utf8(&refs).map_err(ScanError::Utf8)?;
    let ref_names: Vec<_> = refs_text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    let mut grep = Command::new("git");
    grep.arg("-C").arg(&mirror).args([
        "grep",
        "-l",
        "-F",
        "version https://git-lfs.github.com/spec/v1",
    ]);
    grep.args(&ref_names).arg("--");
    let candidates = output_allow_no_matches(&mut grep)?;
    parse_pointer_candidates(&mirror, &candidates).map(|reachable_objects| ReachabilityScan {
        ref_fingerprint,
        reachable_objects,
    })
}

fn parse_pointer_candidates(
    mirror: &std::path::Path,
    candidates: &[u8],
) -> Result<Vec<ObjectOid>, ScanError> {
    let text = std::str::from_utf8(candidates).map_err(ScanError::Utf8)?;
    let mut objects = BTreeSet::new();
    for specification in text.lines().filter(|line| !line.trim().is_empty()) {
        let pointer = output(
            Command::new("git")
                .arg("-C")
                .arg(mirror)
                .args(["show", specification]),
        )?;
        let pointer = std::str::from_utf8(&pointer).map_err(ScanError::Utf8)?;
        let mut lines = pointer.lines();
        if lines.next() != Some("version https://git-lfs.github.com/spec/v1") {
            continue;
        }
        let oid_line = lines
            .next()
            .ok_or_else(|| ScanError::Malformed(specification.into()))?;
        let size_line = lines
            .next()
            .ok_or_else(|| ScanError::Malformed(specification.into()))?;
        if lines.next().is_some() || !size_line.starts_with("size ") {
            return Err(ScanError::Malformed(specification.into()));
        }
        let digest = oid_line
            .strip_prefix("oid sha256:")
            .ok_or_else(|| ScanError::Malformed(specification.into()))?;
        objects.insert(
            digest
                .parse()
                .map_err(|_| ScanError::Malformed(specification.into()))?,
        );
    }
    Ok(objects.into_iter().collect())
}

fn output_allow_no_matches(command: &mut Command) -> Result<Vec<u8>, ScanError> {
    let program = format_command(command);
    let result = command.output()?;
    match result.status.code() {
        Some(0) => Ok(result.stdout),
        Some(1) if result.stdout.is_empty() => Ok(Vec::new()),
        _ => Err(ScanError::Command {
            program,
            status: result.status,
        }),
    }
}

fn run(command: &mut Command) -> Result<(), ScanError> {
    let program = format_command(command);
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(ScanError::Command { program, status })
    }
}

fn output(command: &mut Command) -> Result<Vec<u8>, ScanError> {
    let program = format_command(command);
    let result = command.output()?;
    if result.status.success() {
        Ok(result.stdout)
    } else {
        Err(ScanError::Command {
            program,
            status: result.status,
        })
    }
}

fn format_command(command: &Command) -> String {
    let mut display = command.get_program().to_string_lossy().into_owned();
    for argument in command.get_args() {
        display.push(' ');
        display.push_str(&argument.to_string_lossy());
    }
    display
}

/// Failure to prove a complete Git/LFS reachability snapshot.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Process or temporary-directory I/O failed.
    #[error("reachability scan I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Git or Git LFS returned failure.
    #[error("reachability command `{program}` failed with {status}")]
    Command {
        /// Redacted command display; credentials belong in Git helpers.
        program: String,
        /// Process exit status.
        status: std::process::ExitStatus,
    },
    /// Command output was not UTF-8.
    #[error("reachability output was not UTF-8: {0}")]
    Utf8(#[source] std::str::Utf8Error),
    /// Git LFS emitted a malformed pointer listing.
    #[error("malformed Git LFS listing line: {0}")]
    Malformed(String),
}
