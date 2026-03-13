use aletheiadb::storage::sharding::persistent_commit_log::CommitLogEntry;
use proptest::prelude::*;

#[test]
fn test_oom_dos() {
    let mut data = Vec::new();

    // Entry type
    data.push(1);

    // LSN
    data.extend_from_slice(&0u64.to_le_bytes());

    // TxId
    data.extend_from_slice(&0u64.to_le_bytes());

    // Timestamp
    data.extend_from_slice(&0u64.to_le_bytes());

    // Participant count: MAX -> causes Vec::with_capacity(65535) even if no actual data
    data.extend_from_slice(&u16::MAX.to_le_bytes());

    // Checksum
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&data);
    let checksum = hasher.finalize();
    data.extend_from_slice(&checksum.to_le_bytes());

    // Len
    let mut full_data = Vec::new();
    full_data.extend_from_slice(&(data.len() as u32).to_le_bytes());
    full_data.extend(data);

    let result = CommitLogEntry::deserialize(&full_data);
    assert!(result.is_err());
}

proptest! {
    #[test]
    fn test_deserialize_fuzz(
        entry_type in 1..=3u8,
        lsn in any::<u64>(),
        tx_id in any::<u64>(),
        timestamp in any::<u64>(),
        participant_count in any::<u16>(),
        participants in proptest::collection::vec(any::<u16>(), 0..10),
        has_ts in any::<u8>(),
        ts_bytes in proptest::collection::vec(any::<u8>(), 0..20),
    ) {
        let mut data = Vec::new();
        data.push(entry_type);
        data.extend_from_slice(&lsn.to_le_bytes());
        data.extend_from_slice(&tx_id.to_le_bytes());
        data.extend_from_slice(&timestamp.to_le_bytes());
        data.extend_from_slice(&participant_count.to_le_bytes());
        for p in &participants {
            data.extend_from_slice(&p.to_le_bytes());
        }
        data.push(has_ts);
        data.extend_from_slice(&ts_bytes);

        // checksum
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&data);
        let checksum = hasher.finalize();
        data.extend_from_slice(&checksum.to_le_bytes());

        let mut full_data = Vec::new();
        full_data.extend_from_slice(&(data.len() as u32).to_le_bytes());
        full_data.extend(data);

        let _ = CommitLogEntry::deserialize(&full_data);
    }
}

proptest! {
    #[test]
    fn test_deserialize_crash(data in any::<Vec<u8>>()) {
        let _ = CommitLogEntry::deserialize(&data);
    }
}
