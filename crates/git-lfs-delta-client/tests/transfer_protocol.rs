//! Git LFS custom-transfer protocol integration tests.
#![allow(
    clippy::unwrap_used,
    reason = "literal protocol fixtures should fail immediately when malformed"
)]

use std::io::Cursor;

use git_lfs_delta::{BackendError, DownloadResult, TransferBackend, run_transfer_protocol};

#[derive(Default)]
struct FakeBackend;

impl TransferBackend for FakeBackend {
    fn upload(&self, _request: &git_lfs_delta::UploadRequest) -> Result<u64, BackendError> {
        Ok(12)
    }

    fn download(
        &self,
        _request: &git_lfs_delta::DownloadRequest,
    ) -> Result<DownloadResult, BackendError> {
        Ok(DownloadResult {
            path: "/tmp/git-lfs-delta-download".into(),
            bytes_transferred: 12,
        })
    }
}

#[test]
fn acknowledges_init_and_stops_without_replying_to_terminate() {
    let input = concat!(
        r#"{"event":"init","operation":"upload","remote":"origin","concurrent":false,"concurrenttransfers":8}"#,
        "\n",
        r#"{"event":"terminate"}"#,
        "\n"
    );
    let mut output = Vec::new();

    run_transfer_protocol(Cursor::new(input), &mut output, &FakeBackend).unwrap();

    assert_eq!(String::from_utf8(output).unwrap(), "{}\n");
}

#[test]
fn emits_progress_and_completion_for_an_upload() {
    let input = concat!(
        r#"{"event":"init","operation":"upload","remote":"origin","concurrent":false,"concurrenttransfers":8}"#,
        "\n",
        r#"{"event":"upload","oid":"bf3e3e2af9366a3b704ae0c31de5afa64193ebabffde2091936ad2e7510bc03a","size":12,"path":"/tmp/source","action":{"href":"https://cdc.example/upload","header":{"Authorization":"Bearer test"}}}"#,
        "\n",
        r#"{"event":"terminate"}"#,
        "\n"
    );
    let mut output = Vec::new();

    run_transfer_protocol(Cursor::new(input), &mut output, &FakeBackend).unwrap();

    let lines: Vec<serde_json::Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0], serde_json::json!({}));
    assert_eq!(lines[1]["event"], "progress");
    assert_eq!(lines[1]["bytesSoFar"], 12);
    assert_eq!(lines[2]["event"], "complete");
    assert_eq!(
        lines[2]["oid"],
        "bf3e3e2af9366a3b704ae0c31de5afa64193ebabffde2091936ad2e7510bc03a"
    );
}
