use aletheiadb::api::transaction::visibility::TxVisibilityManager;
use aletheiadb::core::id::TxId;
use aletheiadb::core::Timestamp;

#[test]
fn sentry_epoch_overflow_diff_ts() {
    let mgr = TxVisibilityManager::new();

    // Create an epoch with max offset to cause i64 overflow during linear interpolation
    mgr.register_active(TxId::new(0));
    mgr.register_commit(TxId::new(0), Timestamp::from(i64::MAX));

    mgr.register_active(TxId::new(10));
    mgr.register_commit(TxId::new(10), Timestamp::from(i64::MAX));

    for i in 1..9 {
        mgr.register_active(TxId::new(i));
        mgr.register_commit(TxId::new(i), Timestamp::from(i64::MAX));
    }

    // Compress to create epoch
    mgr.compress_commit_log();

    // Try to get visible timestamp for something in middle
    let snap = mgr.capture_snapshot(Timestamp::from(10));
    let visible = mgr.is_visible(&snap, TxId::new(5));

    // We mainly assert that the above operations did not panic.
    // The timestamp for TxId 5 is i64::MAX, which is > the snapshot timestamp 10,
    // so it should NOT be visible.
    assert!(!visible);
}
