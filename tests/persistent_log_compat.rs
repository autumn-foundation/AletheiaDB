use aletheiadb::storage::sharding::persistent_commit_log::{PersistentCommitLog, CommitLogConfig};
use aletheiadb::storage::sharding::types::ShardId;
use aletheiadb::core::id::TxId;
use std::fs::File;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_v1_compatibility_read() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("commit_v1.log");

    // Manually construct a V1 log file
    let mut file = File::create(&path).unwrap();

    // Header (Magic + Version 1 + Reserved + Entry Count)
    // NOTE: This assumes HEADER_SIZE is 16 bytes.
    // If PersistentCommitLog::HEADER_SIZE changes, this test will break.
    file.write_all(b"GDB2").unwrap();
    file.write_all(&[1u8]).unwrap(); // Version 1
    file.write_all(&[0u8; 3]).unwrap(); // Reserved
    // We will write 1 entry
    file.write_all(&[0u8; 8]).unwrap(); // Entry count (ignored by reader usually, but good to have)

    // Construct V1 Entry: Commit
    // Format: Len(4) + Type(1) + LSN(8) + TxId(8) + Timestamp(8) + PartCount(2) + Parts(N*2) + Checksum(4)
    // NO commit_timestamp

    let mut entry_data = Vec::new();
    entry_data.push(1u8); // Type: Commit
    entry_data.extend_from_slice(&100u64.to_le_bytes()); // LSN
    entry_data.extend_from_slice(&TxId::new(1).as_u64().to_le_bytes()); // TxId
    entry_data.extend_from_slice(&123456789u64.to_le_bytes()); // Timestamp
    entry_data.extend_from_slice(&1u16.to_le_bytes()); // Participant count
    entry_data.extend_from_slice(&ShardId::new(0).unwrap().as_u16().to_le_bytes()); // Shard 0

    // Checksum V1 (CRC32 of entry_data)
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&entry_data);
    let checksum = hasher.finalize();
    entry_data.extend_from_slice(&checksum.to_le_bytes());

    // Write Length + Entry
    let len = entry_data.len() as u32;
    file.write_all(&len.to_le_bytes()).unwrap();
    file.write_all(&entry_data).unwrap();
    file.sync_all().unwrap();
    drop(file);

    // Now try to read it with current PersistentCommitLog
    let log = PersistentCommitLog::new(&path, CommitLogConfig::default()).unwrap();
    let pending = log.pending_commits();

    assert_eq!(pending.len(), 1);
    let entry = &pending[0];
    assert_eq!(entry.tx_id, TxId::new(1));
    assert!(entry.commit_timestamp.is_none(), "V1 entry should have no commit timestamp");
}
