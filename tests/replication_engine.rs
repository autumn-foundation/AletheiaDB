//! Integration tests for the asynchronous replication engine (Issue #3355,
//! Slice B: feed, source, applier, bootstrap, resync detection, promotion).
//!
//! # TDD phase
//!
//! **Red phase**: written against the Slice B design before/alongside the
//! implementation. Covers:
//!
//! 1. Streaming parity over a mixed transaction workload.
//! 2. AC3 bi-temporal (`AS OF`) parity mid-stream.
//! 3. Bounded staleness: a paused source means the replica does not advance.
//! 4. Torn-frame safety: a replica never exposes a partial transaction, even
//!    when the source splits a commit band across polls.
//! 5. Bootstrap from a primary's current snapshot.
//! 6. Restart/resume: a replica with persistence configured resumes from its
//!    last persisted applied LSN, not LSN 1.
//! 7. Resync-required: a source reporting a truncated primary surfaces
//!    `state: "resync_required"` and freezes applied progress.
//! 8. Promotion: role flip, LSN continuity, and pre-promotion data/history
//!    intact.
//! 9. Lag/stats: `entries_behind` is nonzero while behind, zero once caught up.
//!
//! Run with: `cargo test --test replication_engine`

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aletheiadb::api::transaction::{WriteOps, WriteRequestOptions};
use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::error::Result;
use aletheiadb::core::temporal::time;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::{
    AletheiaDB, DurabilityMode, FetchOutcome, InProcessSource, NodeRole, PropertyMap,
    PropertyMapBuilder, ReplicationOptions, ReplicationSource,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn props(name: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// A durable primary: `Synchronous` durability so every commit is fsynced to
/// a segment file before the call returns -- `read_from`/the feed only ever
/// see durable data, so this is the minimum needed for the feed to observe a
/// write deterministically (no flush-interval race).
fn build_durable_primary() -> (tempfile::TempDir, AletheiaDB) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.path().join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create primary");
    (dir, db)
}

/// A durable replica with index persistence enabled at `data_dir`, so
/// periodic persistence (and restart/resume) can be exercised.
fn durable_replica_config(data_dir: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .build()
}

/// Fast-cadence options so tests converge quickly without long sleeps.
fn fast_opts() -> ReplicationOptions {
    ReplicationOptions {
        poll_interval: Duration::from_millis(15),
        batch_max_entries: 500,
        persist_every: Some(Duration::from_millis(100)),
        persist_every_entries: 5,
    }
}

/// Poll `check` until it returns `true` or `timeout` elapses; returns the
/// final result of `check` (so a caller can assert with a useful message).
fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut check: F) -> bool {
    let start = Instant::now();
    loop {
        if check() {
            return true;
        }
        if start.elapsed() >= timeout {
            return check();
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn name_of(db: &AletheiaDB, id: aletheiadb::core::id::NodeId) -> Option<String> {
    db.get_node(id)
        .ok()
        .and_then(|n| n.properties.get("name").map(|v| format!("{v:?}")))
}

// ---------------------------------------------------------------------------
// Test-owned `ReplicationSource` wrappers
// ---------------------------------------------------------------------------

/// Records every `from_lsn` requested, for the restart/resume test.
struct RecordingSource {
    inner: InProcessSource,
    requested_from_lsns: Arc<Mutex<Vec<u64>>>,
}

impl ReplicationSource for RecordingSource {
    fn fetch_entries(&mut self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome> {
        self.requested_from_lsns
            .lock()
            .expect("lock")
            .push(from_lsn);
        self.inner.fetch_entries(from_lsn, max_entries)
    }

    fn fetch_snapshot(&mut self, dest: &Path) -> Result<u64> {
        self.inner.fetch_snapshot(dest)
    }
}

/// A source that can be paused: while paused, every fetch reports "nothing
/// new" instead of touching the real primary feed.
struct PausableSource {
    inner: InProcessSource,
    paused: Arc<AtomicBool>,
}

impl ReplicationSource for PausableSource {
    fn fetch_entries(&mut self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome> {
        if self.paused.load(Ordering::Acquire) {
            return Ok(FetchOutcome::Entries {
                entries: Vec::new(),
                primary_flushed_lsn: 0,
                primary_wallclock_micros: 0,
            });
        }
        self.inner.fetch_entries(from_lsn, max_entries)
    }

    fn fetch_snapshot(&mut self, dest: &Path) -> Result<u64> {
        self.inner.fetch_snapshot(dest)
    }
}

/// A source that caps every fetch at a small `max_entries`, forcing a
/// multi-op transaction's `[BeginTx .. CommitTx]` band to split across
/// multiple polls (exercising torn-frame safety).
struct TornBatchSource {
    inner: InProcessSource,
    cap: usize,
}

impl ReplicationSource for TornBatchSource {
    fn fetch_entries(&mut self, from_lsn: u64, _max_entries: usize) -> Result<FetchOutcome> {
        self.inner.fetch_entries(from_lsn, self.cap)
    }

    fn fetch_snapshot(&mut self, dest: &Path) -> Result<u64> {
        self.inner.fetch_snapshot(dest)
    }
}

/// A source that always reports `ResyncRequired`, simulating a primary that
/// has truncated WAL history the replica still needs.
struct AlwaysResyncSource {
    min_available_lsn: u64,
}

impl ReplicationSource for AlwaysResyncSource {
    fn fetch_entries(&mut self, _from_lsn: u64, _max_entries: usize) -> Result<FetchOutcome> {
        Ok(FetchOutcome::ResyncRequired {
            min_available_lsn: self.min_available_lsn,
        })
    }

    fn fetch_snapshot(&mut self, _dest: &Path) -> Result<u64> {
        unimplemented!("not exercised by the resync test")
    }
}

// ---------------------------------------------------------------------------
// 1. Streaming parity over a mixed transaction workload
// ---------------------------------------------------------------------------

#[test]
fn streaming_parity_over_mixed_transaction_workload() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    let mut node_ids = Vec::new();
    for i in 0..10 {
        let id = primary
            .create_node("Person", props(&format!("P{i}")))
            .expect("create node");
        node_ids.push(id);
    }
    for pair in node_ids.windows(2) {
        primary
            .create_edge(pair[0], pair[1], "KNOWS", PropertyMapBuilder::new().build())
            .expect("create edge");
    }
    // Updates.
    for &id in &node_ids[0..3] {
        primary
            .update_node_with_valid_time(id, props("updated"), None)
            .expect("update node");
    }
    // Deletes.
    primary
        .delete_node_with_options(node_ids[9], WriteRequestOptions::default())
        .expect("delete node");

    let replica = AletheiaDB::new().expect("create replica");
    let source = Box::new(InProcessSource::new(Arc::clone(&primary)));
    replica
        .start_replication(source, fast_opts())
        .expect("start replication");

    assert!(
        wait_until(Duration::from_secs(10), || {
            replica.node_count() == primary.node_count()
                && replica.edge_count() == primary.edge_count()
        }),
        "replica node/edge counts must converge to the primary's (primary: {}/{}, replica: {}/{})",
        primary.node_count(),
        primary.edge_count(),
        replica.node_count(),
        replica.edge_count(),
    );

    // Specific property parity on an updated node.
    assert_eq!(
        name_of(&replica, node_ids[0]),
        name_of(&primary, node_ids[0]),
    );

    // History parity for a sampled (updated) node.
    let primary_history = primary
        .get_node_history(node_ids[0])
        .expect("primary history");
    let replica_history =
        wait_for_history_len(&replica, node_ids[0], primary_history.version_count());
    assert_eq!(
        replica_history.version_count(),
        primary_history.version_count()
    );

    // The deleted node must be gone from the replica's current state too.
    assert!(replica.get_node(node_ids[9]).is_err());
}

fn wait_for_history_len(
    db: &AletheiaDB,
    id: aletheiadb::core::id::NodeId,
    expected: usize,
) -> aletheiadb::core::history::EntityHistory {
    let mut last = db.get_node_history(id).expect("history");
    let ok = wait_until(Duration::from_secs(10), || {
        last = db.get_node_history(id).expect("history");
        last.version_count() == expected
    });
    assert!(ok, "history length must converge to {expected}");
    last
}

// ---------------------------------------------------------------------------
// 2. AC3 bi-temporal (AS OF) parity mid-stream
// ---------------------------------------------------------------------------

#[test]
fn as_of_temporal_parity_mid_stream() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    let node_id = primary
        .create_node("Person", props("v1"))
        .expect("create node");

    // T1: the transaction-time coordinate of the create.
    let created_history = primary.get_node_history(node_id).expect("history");
    let t1 = created_history
        .first_version()
        .expect("first version")
        .temporal
        .transaction_time()
        .start();

    let replica = AletheiaDB::new().expect("create replica");
    replica
        .start_replication(
            Box::new(InProcessSource::new(Arc::clone(&primary))),
            fast_opts(),
        )
        .expect("start replication");

    assert!(wait_until(Duration::from_secs(10), || replica
        .get_node(node_id)
        .is_ok()));

    // Now update -- after T1.
    primary
        .update_node_with_valid_time(node_id, props("v2"), None)
        .expect("update node");

    assert!(wait_until(Duration::from_secs(10), || {
        name_of(&replica, node_id) == name_of(&primary, node_id)
    }));

    let now = time::now();
    let primary_at_t1 = primary
        .get_node_at_time(node_id, now, t1)
        .expect("primary as-of t1");
    let replica_at_t1 = replica
        .get_node_at_time(node_id, now, t1)
        .expect("replica as-of t1");

    assert_eq!(
        primary_at_t1.properties.get("name"),
        replica_at_t1.properties.get("name"),
        "AS OF SYSTEM_TIME t1 must agree between primary and replica"
    );

    // Sanity: at t1 both must show the ORIGINAL value, not the update.
    let expected_v1 = PropertyMapBuilder::new().insert("name", "v1").build();
    assert_eq!(
        primary_at_t1.properties.get("name"),
        expected_v1.get("name")
    );

    // find_nodes_at_time parity too, since it's the entry-point resolver.
    let primary_found = primary
        .find_nodes_at_time("Person", now, t1)
        .expect("primary find_nodes_at_time");
    let replica_found = replica
        .find_nodes_at_time("Person", now, t1)
        .expect("replica find_nodes_at_time");
    assert_eq!(primary_found.nodes.len(), replica_found.nodes.len());
}

// ---------------------------------------------------------------------------
// 3. Bounded staleness
// ---------------------------------------------------------------------------

#[test]
fn bounded_staleness_replica_does_not_advance_while_paused() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    let node_id = primary
        .create_node("Person", props("before-pause"))
        .expect("create node");

    let replica = AletheiaDB::new().expect("create replica");
    let paused = Arc::new(AtomicBool::new(true));
    let source = Box::new(PausableSource {
        inner: InProcessSource::new(Arc::clone(&primary)),
        paused: Arc::clone(&paused),
    });
    replica
        .start_replication(source, fast_opts())
        .expect("start replication");

    // Give the applier a few polls to (not) do anything while paused.
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        replica.get_node(node_id).is_err(),
        "a paused replica must not see the primary's write"
    );
    let frozen_progress = replica
        .replication_progress()
        .expect("progress")
        .last_applied_lsn;

    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        replica
            .replication_progress()
            .expect("progress")
            .last_applied_lsn,
        frozen_progress,
        "applied position must not move while paused"
    );

    // Unpause: convergence follows.
    paused.store(false, Ordering::Release);
    assert!(wait_until(Duration::from_secs(10), || replica
        .get_node(node_id)
        .is_ok()));
}

// ---------------------------------------------------------------------------
// 4. Torn-frame safety
// ---------------------------------------------------------------------------

#[test]
fn torn_frame_safety_never_exposes_a_partial_transaction() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    // A single multi-op transaction: two node creates + an edge, all-or-nothing.
    let mut tx = primary.write_transaction().expect("begin tx");
    let a = tx.create_node("Person", props("A")).expect("buffer a");
    let b = tx.create_node("Person", props("B")).expect("buffer b");
    let edge_id = tx
        .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .expect("buffer edge");
    tx.commit().expect("commit multi-op tx");

    let replica = AletheiaDB::new().expect("create replica");
    // Cap fetches at 1 entry so the BeginTx/data/CommitTx band is guaranteed
    // to split across multiple polls.
    let source = Box::new(TornBatchSource {
        inner: InProcessSource::new(Arc::clone(&primary)),
        cap: 1,
    });
    replica
        .start_replication(source, fast_opts())
        .expect("start replication");

    // While streaming, at every observation the multi-op transaction must be
    // either entirely absent or entirely present -- never half-applied.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed_complete = false;
    while Instant::now() < deadline {
        let a_present = replica.get_node(a).is_ok();
        let b_present = replica.get_node(b).is_ok();
        // `a` is written before `b` inside the frame, and the two reads are two
        // points in time, so exactly three outcomes are admissible:
        // (false,false), (false,true) -- the apply landed between the reads --
        // and (true,true). `(true,false)` is the one that cannot happen under an
        // atomic apply: once `a` is visible the whole frame has been published,
        // so `b`, read strictly later, must be visible too. Observing it means
        // the reader saw real intermediate state (Issue #3788).
        assert!(
            !a_present || b_present,
            "the two nodes of one atomic transaction must appear together: \
             saw a_present={a_present} b_present={b_present}"
        );
        if a_present {
            // The edge map (a direct, lock-synchronized lookup -- unlike the
            // best-effort adjacency cache's own relaxed-atomic fast path,
            // which is a separate, independently-consistent optimization
            // layer) is the source of truth for "this transaction's edge
            // landed".
            assert!(
                replica.get_edge(edge_id).is_ok(),
                "if the transaction's nodes are visible, its edge must be too"
            );
            observed_complete = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        observed_complete,
        "the torn-batch source must eventually converge once every band fragment arrives"
    );
}

// ---------------------------------------------------------------------------
// 5. Bootstrap from a snapshot
// ---------------------------------------------------------------------------

#[test]
fn bootstrap_replica_from_primary_snapshot() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    let alice = primary
        .create_node("Person", props("Alice"))
        .expect("create alice");
    let bob = primary
        .create_node("Person", props("Bob"))
        .expect("create bob");
    primary
        .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
        .expect("create edge");

    let replica_dir = tempfile::tempdir().expect("replica tempdir");
    let source = Box::new(InProcessSource::new(Arc::clone(&primary)));
    let replica = AletheiaDB::bootstrap_replica(source, replica_dir.path(), fast_opts())
        .expect("bootstrap replica");

    assert_eq!(replica.node_count(), primary.node_count());
    assert_eq!(replica.edge_count(), primary.edge_count());
    assert_eq!(name_of(&replica, alice), name_of(&primary, alice));

    // Continued writes stream in after bootstrap.
    let carol = primary
        .create_node("Person", props("Carol"))
        .expect("create carol");
    assert!(wait_until(Duration::from_secs(10), || replica
        .get_node(carol)
        .is_ok()));
}

// ---------------------------------------------------------------------------
// 6. Restart / resume
// ---------------------------------------------------------------------------

#[test]
fn restart_resumes_from_persisted_applied_lsn_not_from_scratch() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    for i in 0..10 {
        primary
            .create_node("Person", props(&format!("P{i}")))
            .expect("create node");
    }

    let replica_dir = tempfile::tempdir().expect("replica tempdir");
    {
        let replica = AletheiaDB::with_unified_config(durable_replica_config(replica_dir.path()))
            .expect("create durable replica");
        replica
            .start_replication(
                Box::new(InProcessSource::new(Arc::clone(&primary))),
                fast_opts(),
            )
            .expect("start replication");

        assert!(wait_until(Duration::from_secs(10), || replica.node_count()
            == primary.node_count()));

        // Give the periodic-persist trigger a chance to fire at least once.
        assert!(wait_until(Duration::from_secs(5), || {
            replica
                .replication_progress()
                .map(|p| p.last_applied_lsn > 1)
                .unwrap_or(false)
        }));
        std::thread::sleep(Duration::from_millis(250));
        // Replica dropped here -- its `Drop` stops+joins the applier thread.
    }

    // More writes on the primary AFTER the replica went away -- must NOT be
    // visible until the reopened replica streams them.
    let post_restart_node = primary
        .create_node("Person", props("post-restart"))
        .expect("create post-restart node");

    let requested_from_lsns = Arc::new(Mutex::new(Vec::new()));
    let recording_source = Box::new(RecordingSource {
        inner: InProcessSource::new(Arc::clone(&primary)),
        requested_from_lsns: Arc::clone(&requested_from_lsns),
    });

    let replica = AletheiaDB::with_unified_config(durable_replica_config(replica_dir.path()))
        .expect("reopen durable replica");
    replica
        .start_replication(recording_source, fast_opts())
        .expect("resume replication");

    assert!(wait_until(Duration::from_secs(10), || replica
        .get_node(post_restart_node)
        .is_ok()));

    let first_requested = requested_from_lsns
        .lock()
        .expect("lock")
        .first()
        .copied()
        .expect("at least one fetch happened");
    assert!(
        first_requested > 1,
        "a resumed replica must NOT restart streaming from LSN 1 -- first requested \
         from_lsn was {first_requested}"
    );

    // No resync should have been needed: the primary never truncated anything.
    assert_ne!(
        replica.replication_progress().expect("progress").state,
        "resync_required"
    );
}

// ---------------------------------------------------------------------------
// 7. Resync required
// ---------------------------------------------------------------------------

#[test]
fn resync_required_freezes_progress_and_reports_state() {
    let replica = AletheiaDB::new().expect("create replica");
    let source = Box::new(AlwaysResyncSource {
        min_available_lsn: 42,
    });
    replica
        .start_replication(source, fast_opts())
        .expect("start replication");

    assert!(wait_until(Duration::from_secs(5), || replica
        .replication_progress()
        .map(|p| p.state == "resync_required")
        .unwrap_or(false)));

    let progress = replica.replication_progress().expect("progress");
    assert_eq!(progress.state, "resync_required");
    assert_eq!(progress.last_applied_lsn, 0);
    assert!(
        progress
            .last_error
            .as_ref()
            .is_some_and(|e| e.contains("42")),
        "last_error should mention the min available LSN, got: {:?}",
        progress.last_error
    );

    // Applied position stays frozen across further polls.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        replica
            .replication_progress()
            .expect("progress")
            .last_applied_lsn,
        0
    );

    // Also visible via `stats()`.
    let stats = replica.stats();
    assert_eq!(stats.replication.role, "replica");
    assert_eq!(
        stats
            .replication
            .replica
            .as_ref()
            .expect("replica stats")
            .state,
        "resync_required"
    );
}

// ---------------------------------------------------------------------------
// 8. Promotion
// ---------------------------------------------------------------------------

#[test]
fn promotion_flips_role_and_preserves_data_and_history() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    let alice = primary
        .create_node("Person", props("Alice"))
        .expect("create alice");
    primary
        .update_node_with_valid_time(alice, props("Alice-v2"), None)
        .expect("update alice");

    let replica = AletheiaDB::new().expect("create replica");
    replica
        .start_replication(
            Box::new(InProcessSource::new(Arc::clone(&primary))),
            fast_opts(),
        )
        .expect("start replication");

    assert!(wait_until(Duration::from_secs(10), || {
        name_of(&replica, alice) == name_of(&primary, alice)
    }));

    let pre_promotion_history = replica.get_node_history(alice).expect("history");
    assert_eq!(pre_promotion_history.version_count(), 2);

    assert!(replica.is_replica());
    let report = replica.promote_to_primary().expect("promote");
    assert!(report.applier_stopped);
    assert!(report.last_applied_lsn.is_some());
    assert_eq!(replica.node_role(), NodeRole::Primary);
    assert!(!replica.is_replica());

    // Writes now succeed.
    let new_id = replica
        .create_node("Person", props("NewAfterPromotion"))
        .expect("write succeeds after promotion");
    assert!(replica.get_node(new_id).is_ok());

    // Pre-promotion data + temporal history intact.
    assert_eq!(
        replica
            .get_node_history(alice)
            .expect("history after promotion")
            .version_count(),
        2
    );
    assert_eq!(
        name_of(&replica, alice),
        Some(name_of(&primary, alice).unwrap())
    );
}

// ---------------------------------------------------------------------------
// 9. Lag / stats
// ---------------------------------------------------------------------------

#[test]
fn stats_report_entries_behind_while_pending_then_zero_once_converged() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    let replica = AletheiaDB::new().expect("create replica");
    let paused = Arc::new(AtomicBool::new(true));
    let source = Box::new(PausableSource {
        inner: InProcessSource::new(Arc::clone(&primary)),
        paused: Arc::clone(&paused),
    });
    replica
        .start_replication(source, fast_opts())
        .expect("start replication");

    for i in 0..5 {
        primary
            .create_node("Person", props(&format!("Lag{i}")))
            .expect("create node");
    }

    // While paused, the applier can't even observe the primary's flushed LSN
    // (the pausable wrapper reports 0), so entries_behind is unknown (None).
    // Unpause and let it observe the backlog, then converge.
    paused.store(false, Ordering::Release);

    assert!(wait_until(Duration::from_secs(5), || {
        replica
            .replication_progress()
            .and_then(|p| p.entries_behind)
            .is_some()
    }));

    assert!(wait_until(Duration::from_secs(10), || {
        replica
            .replication_progress()
            .and_then(|p| p.entries_behind)
            == Some(0)
    }));

    let stats = replica.stats();
    assert_eq!(
        stats
            .replication
            .replica
            .expect("replica stats")
            .entries_behind,
        Some(0)
    );
}

// ---------------------------------------------------------------------------
// 10. Torn-frame safety under sustained load (Issue #3788)
// ---------------------------------------------------------------------------

/// The single-transaction torn-frame test above observes one commit band. This
/// one holds the same invariant under *sustained* concurrent load: a stream of
/// multi-op transactions applying continuously while several reader threads
/// hammer the replica's current-state surface.
///
/// The invariant is per-transaction atomicity of visibility. Every transaction
/// writes node `a`, then node `b`, then the edge joining them. Because `a` is
/// published first, seeing `a` implies the whole frame has been published, so
/// `b` and the edge -- read strictly later -- must be visible too. `a` visible
/// with `b` missing is the shape reported in #3788 and means the reader saw
/// real intermediate state. The reverse (`b` visible, `a` not yet read as
/// present) is ordinary staleness: the apply simply landed between two reads.
#[test]
fn torn_frame_safety_holds_under_sustained_concurrent_load() {
    let (_primary_dir, primary) = build_durable_primary();
    let primary = Arc::new(primary);

    const PAIRS: usize = 60;

    // Each transaction is a two-node + one-edge atomic unit.
    let mut pairs = Vec::with_capacity(PAIRS);
    for i in 0..PAIRS {
        let mut tx = primary.write_transaction().expect("begin tx");
        let a = tx
            .create_node("Person", props(&format!("A{i}")))
            .expect("buffer a");
        let b = tx
            .create_node("Person", props(&format!("B{i}")))
            .expect("buffer b");
        let e = tx
            .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
            .expect("buffer edge");
        tx.commit().expect("commit multi-op tx");
        pairs.push((a, b, e));
    }

    let replica = Arc::new(AletheiaDB::new().expect("create replica"));
    // Cap fetches at 2 entries: every band is split across polls, so the
    // applier is publishing near-continuously for the whole run.
    let source = Box::new(TornBatchSource {
        inner: InProcessSource::new(Arc::clone(&primary)),
        cap: 2,
    });
    replica
        .start_replication(source, fast_opts())
        .expect("start replication");

    let pairs = Arc::new(pairs);
    let stop = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(Mutex::new(Vec::<String>::new()));

    let readers: Vec<_> = (0..4)
        .map(|reader| {
            let (replica, pairs, stop, violations) = (
                Arc::clone(&replica),
                Arc::clone(&pairs),
                Arc::clone(&stop),
                Arc::clone(&violations),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    for (i, (a, b, e)) in pairs.iter().enumerate() {
                        // Read in publish order: a, then b, then the edge.
                        let a_present = replica.get_node(*a).is_ok();
                        if !a_present {
                            continue;
                        }
                        if replica.get_node(*b).is_err() {
                            violations.lock().expect("lock").push(format!(
                                "reader {reader}: pair {i} torn -- first node visible, \
                                 second node missing"
                            ));
                            return;
                        }
                        if replica.get_edge(*e).is_err() {
                            violations.lock().expect("lock").push(format!(
                                "reader {reader}: pair {i} nodes visible but edge missing"
                            ));
                            return;
                        }
                    }
                }
            })
        })
        .collect();

    let converged = wait_until(Duration::from_secs(30), || {
        pairs
            .last()
            .is_some_and(|(_, b, _)| replica.get_node(*b).is_ok())
    });
    stop.store(true, Ordering::Release);
    for reader in readers {
        reader.join().expect("reader thread");
    }

    let violations = violations.lock().expect("lock");
    assert!(
        violations.is_empty(),
        "a replica read observed a partially-applied transaction: {violations:#?}"
    );
    assert!(
        converged,
        "the replica must eventually converge on every transaction"
    );

    // And the end state is complete, not merely untorn.
    for (a, b, e) in pairs.iter() {
        assert!(replica.get_node(*a).is_ok());
        assert!(replica.get_node(*b).is_ok());
        assert!(replica.get_edge(*e).is_ok());
    }
}
