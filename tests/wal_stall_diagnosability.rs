//! Issue #3798: a stalled WAL must be *diagnosable*, never a silent hang.
//!
//! Two failure modes are covered here, both through the public API only:
//!
//! 1. **Writers block forever.** When the background flush thread stops
//!    draining, every stripe ring buffer fills and each `append` parks
//!    indefinitely. A caller cannot tell that from a hung process, so the
//!    append must instead fail with an error naming what filled up and where
//!    to look (`regression_wal_append_fails_diagnosably_when_flusher_is_wedged`).
//!
//! 2. **The flusher dies silently.** Nothing outside the flush thread can
//!    observe whether it is still running, so a monitor has no signal to alarm
//!    on (`wal_heartbeat_is_publicly_observable`).
//!
//! Every cross-thread assertion runs under a watchdog: these tests fail fast
//! with an explicit message instead of hanging the suite.

use aletheiadb::GLOBAL_INTERNER;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use aletheiadb::storage::wal::{DurabilityMode, WalOperation};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Upper bound on any bounded poll in this file.
const WATCHDOG: Duration = Duration::from_secs(5);

fn create_test_operation(id: u64) -> WalOperation {
    WalOperation::CreateNode {
        node_id: NodeId::new(id).unwrap(),
        label: GLOBAL_INTERNER.intern(format!("Node{}", id)).unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    }
}

/// Poll `cond` every 10ms until it holds or `budget` elapses.
fn poll_until(mut cond: impl FnMut() -> bool, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn group_commit_config(dir: &std::path::Path, flush_interval_ms: u64) -> ConcurrentWalSystemConfig {
    ConcurrentWalSystemConfig::new(dir)
        .with_durability_mode(DurabilityMode::GroupCommit {
            max_batch_size: 8,
            max_delay_ms: 5,
        })
        .with_flush_interval_ms(flush_interval_ms)
}

/// With the flusher wedged and the ring buffer full, an append must FAIL with
/// a diagnosable error rather than blocking the writer forever.
#[test]
fn regression_wal_append_fails_diagnosably_when_flusher_is_wedged() {
    let dir = tempdir().unwrap();
    // One stripe, two slots, and a flusher that runs its startup cycle and
    // then sleeps for an hour: wedged by configuration, so nothing will ever
    // drain the buffer again.
    let mut config = group_commit_config(dir.path(), 3_600_000)
        .with_num_stripes(1)
        .with_max_append_block_ms(200);
    config.stripe_capacity = 2;

    let wal = Arc::new(ConcurrentWalSystem::new(config).unwrap());

    // Let that startup cycle happen before we start filling the buffer. The
    // heartbeat is the precise signal; while it is not published yet, fall
    // back to a fixed grace period rather than failing THIS test for the wrong
    // reason -- heartbeat observability is asserted by
    // `wal_heartbeat_is_publicly_observable` below.
    if !poll_until(|| wal.flush_heartbeat() >= 1, WATCHDOG) {
        std::thread::sleep(Duration::from_millis(200));
    }

    let (tx, rx) = mpsc::channel();
    let worker_wal = Arc::clone(&wal);
    // Detached on purpose: while appends block forever this worker never
    // returns, so the TEST must never join it -- the watchdog below decides.
    std::thread::spawn(move || {
        // capacity (2) + 4 attempts: well past the point where an undrained
        // ring buffer has to refuse.
        for i in 0..6u64 {
            if let Err(e) = worker_wal.append(create_test_operation(i + 1)) {
                let _ = tx.send(Some(e.to_string()));
                return;
            }
        }
        let _ = tx.send(None);
    });

    let outcome = rx.recv_timeout(Duration::from_secs(10)).expect(
        "WAL append never returned within 10s: with a wedged flusher and a full 2-slot ring \
         buffer, writers block forever instead of failing (Issue #3798)",
    );

    let message = outcome.expect(
        "every append succeeded against a wedged flusher -- the blocking-append bound was \
         never applied",
    );
    assert!(
        message.contains("ring buffer full"),
        "the error must name what filled up, got: {message}"
    );
    assert!(
        message.contains("background flusher"),
        "the error must point the operator at the flusher, got: {message}"
    );

    // The system must still be usable (nothing panicked or poisoned).
    let _ = wal.flush_heartbeat();
}

/// The flusher's liveness must be observable from OUTSIDE the crate.
///
/// Deliberately duplicates the in-crate `test_flush_heartbeat_advances` unit
/// test at the public-API level: an operator or health monitor only has this
/// surface to alarm on.
#[test]
fn wal_heartbeat_is_publicly_observable() {
    let dir = tempdir().unwrap();
    let wal = ConcurrentWalSystem::new(group_commit_config(dir.path(), 10)).unwrap();

    assert!(
        poll_until(|| wal.flush_heartbeat() >= 1, WATCHDOG),
        "flush_heartbeat() never reached 1 within {WATCHDOG:?} with a 10ms flush interval: \
         the running flusher publishes no liveness signal"
    );

    let first = wal.flush_heartbeat();
    assert!(
        poll_until(|| wal.flush_heartbeat() > first, WATCHDOG),
        "flush_heartbeat() stuck at {first} within {WATCHDOG:?}: the signal does not advance \
         once per flush cycle"
    );
}
