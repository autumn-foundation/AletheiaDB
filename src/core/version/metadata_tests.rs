use super::*;

#[test]
fn test_version_metadata_new() {
    let tx_id = TxId::new(100);
    let timestamp = Timestamp::from(5000);
    let metadata = VersionMetadata::new(tx_id, timestamp);

    assert_eq!(metadata.created_by_tx, tx_id);
    assert_eq!(metadata.commit_timestamp, Some(timestamp));
}

#[test]
fn test_version_metadata_uncommitted() {
    let tx_id = TxId::new(200);
    let metadata = VersionMetadata::uncommitted(tx_id);

    assert_eq!(metadata.created_by_tx, tx_id);
    assert_eq!(metadata.commit_timestamp, None);
}

#[test]
fn test_version_metadata_default() {
    use std::process::Command;
    use std::time::{Duration, Instant};

    let exe = std::env::current_exe().expect("failed to locate current test binary");
    let mut child = Command::new(exe)
        .args([
            "--ignored",
            "--exact",
            "core::version::metadata_tests::test_version_metadata_default_subprocess_helper",
        ])
        .spawn()
        .expect("failed to spawn subprocess for default metadata test");

    // CI environments can be slow, so give it plenty of time (10s)
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(
                    status.success(),
                    "subprocess helper failed for VersionMetadata default semantics"
                );
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("VersionMetadata::default/default_for_existing did not complete");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("failed while polling subprocess: {e}"),
        }
    }
}

#[test]
#[ignore]
fn test_version_metadata_default_subprocess_helper() {
    let metadata = VersionMetadata::default();
    let default_expected = VersionMetadata::default_for_existing();

    assert_eq!(metadata.created_by_tx, default_expected.created_by_tx);
    assert_eq!(metadata.commit_timestamp, default_expected.commit_timestamp);
    assert_eq!(metadata.created_by_tx, TxId::new(0));
    assert!(metadata.commit_timestamp.is_some());
}

#[test]
fn test_version_metadata_debug() {
    let tx_id = TxId::new(123);
    let timestamp = Timestamp::from(456);
    let metadata = VersionMetadata::new(tx_id, timestamp);
    let debug_str = format!("{:?}", metadata);

    assert!(debug_str.contains("VersionMetadata"));
    assert!(debug_str.contains("created_by_tx"));
    assert!(debug_str.contains("commit_timestamp"));
    assert!(debug_str.contains("123"));
}

#[test]
fn test_version_metadata_clone_copy() {
    let tx_id = TxId::new(123);
    let timestamp = Timestamp::from(456);
    let metadata = VersionMetadata::new(tx_id, timestamp);

    let copy = metadata; // Copy
    assert_eq!(metadata, copy);

    #[allow(clippy::clone_on_copy)]
    let clone = metadata.clone(); // Clone
    assert_eq!(metadata, clone);
}
