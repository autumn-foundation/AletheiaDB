use aletheiadb::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};

#[test]
fn test_warden_wal_dos_stripe_capacity() {
    // 🛡️ Warden: Test DoS vector via excessive stripe capacity
    const HUGE_CAPACITY: usize = 100_000_000;

    // We create a temporary directory for the WAL
    let dir = tempfile::tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_stripe_capacity(HUGE_CAPACITY);

    // ConcurrentWal::new should fail
    let result = ConcurrentWal::new(config);

    assert!(result.is_err(), "Should reject huge stripe capacity");
    // Optionally check error message
}

#[test]
fn test_warden_wal_dos_num_stripes() {
    // 🛡️ Warden: Test DoS vector via excessive stripes
    const HUGE_STRIPES: usize = 1_000_000;

    let dir = tempfile::tempdir().unwrap();
    let config = ConcurrentWalConfig::new(dir.path()).with_num_stripes(HUGE_STRIPES);

    let result = ConcurrentWal::new(config);

    assert!(result.is_err(), "Should reject huge number of stripes");
}
