//! Contract tests against the public Git LFS JSON shape.
#![allow(
    clippy::unwrap_used,
    reason = "literal wire fixtures should fail immediately when malformed"
)]

use git_lfs_delta_protocol::{BatchRequest, Operation, TransferKind, select_transfer};

#[test]
fn parses_a_stock_git_lfs_upload_batch() {
    let request: BatchRequest = serde_json::from_str(
        r#"{
          "operation": "upload",
          "transfers": ["basic"],
          "ref": {"name": "refs/heads/master"},
          "objects": [{
            "oid": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "size": 0
          }]
        }"#,
    )
    .unwrap();

    assert_eq!(request.operation, Operation::Upload);
    assert_eq!(request.transfers, vec!["basic"]);
    assert_eq!(request.reference.unwrap().name, "refs/heads/master");
    assert_eq!(request.objects.len(), 1);
}

#[test]
fn absent_transfer_list_means_basic() {
    assert_eq!(select_transfer(&[]).unwrap(), TransferKind::Basic);
}

#[test]
fn cdc_is_selected_only_when_the_client_offers_it() {
    assert_eq!(
        select_transfer(&["basic".into(), "cdc".into()]).unwrap(),
        TransferKind::Cdc
    );
    assert_eq!(
        select_transfer(&["basic".into()]).unwrap(),
        TransferKind::Basic
    );
}

#[test]
fn unsupported_only_transfer_lists_fail_instead_of_silently_downgrading() {
    assert!(select_transfer(&["future-transfer".into()]).is_err());
}
