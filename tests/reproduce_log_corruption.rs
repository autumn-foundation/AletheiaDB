
use aletheiadb::storage::sharding::persistent_commit_log::{PersistentCommitLog, CommitLogConfig};
use aletheiadb::core::id::TxId;
use aletheiadb::storage::sharding::types::ShardId;
use std::io::Write;
use std::fs::OpenOptions;

#[test]
fn test_reproduce_log_corruption_data_loss() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("commit.log");

    // 1. Create a log and write some entries
    {
        let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
        log.log_commit(TxId::new(1), vec![ShardId::new(0).unwrap()], None).unwrap();
        log.close().unwrap();
    }

    // 2. Corrupt the log by appending garbage
    {
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"garbage_data_that_is_not_a_valid_entry").unwrap();
        file.sync_all().unwrap();
    }

    // 3. Reopen the log and write a NEW entry
    // If the bug exists, this entry will be appended AFTER the garbage.
    {
        let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
        // The log might or might not panic here depending on implementation,
        // but the current read_entries implementation swallows errors at the end.

        // Verify we still see the first entry
        assert!(log.get_decision(TxId::new(1)).is_some(), "Should recover first entry");

        // Write new entry
        log.log_commit(TxId::new(2), vec![ShardId::new(0).unwrap()], None).unwrap();
        log.close().unwrap();
    }

    // 4. Reopen again and check for the new entry
    {
        let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();

        // Verify first entry is still there
        assert!(log.get_decision(TxId::new(1)).is_some(), "First entry should persist");

        // Verify second entry is there.
        // WITH THE BUG: This assertion will FAIL because read_entries stops at the garbage.
        assert!(log.get_decision(TxId::new(2)).is_some(), "Second entry should persist despite previous corruption");
    }
}
