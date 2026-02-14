use aletheiadb::core::id::NodeId;
use aletheiadb::core::interning::GLOBAL_INTERNER;
use aletheiadb::core::property::{PropertyMapBuilder, PropertyValue};
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::WalOperation;
use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use tempfile::tempdir;

#[test]
fn test_wal_entry_size_limit() {
    let dir = tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path());
    let wal = ConcurrentWal::new(config).unwrap();

    // Create a huge property value
    // MAX_WAL_ENTRY_SIZE is 64MB.
    // We create a value slightly larger than that.
    let size = 65 * 1024 * 1024; // 65 MB
    let huge_bytes = vec![0u8; size];
    let huge_prop = PropertyValue::bytes(huge_bytes);

    let properties = PropertyMapBuilder::new().insert("huge", huge_prop).build();

    let op = WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("DoS").unwrap(),
        properties,
        valid_from: time::now(),
    };

    // Attempt to append
    let result = wal.append_async(op);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Capacity exceeded for WAL entry size"));
    assert!(err.contains("DoS protection"));
}
