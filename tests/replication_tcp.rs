//! Integration tests for the TCP replication transport (Issue #3355, Slice
//! C: framed wire protocol, token auth, snapshot streaming, config wiring,
//! reconnect/resume, resync detection).
//!
//! # TDD phase
//!
//! **Red phase**: written against the Slice C design before/alongside the
//! implementation. Covers:
//!
//! 1. Handshake: wrong token never reaches `Streaming`; correct token streams.
//! 2. TCP streaming parity over a mixed transaction workload.
//! 3. Snapshot bootstrap over TCP, then continued streaming.
//! 4. Reconnect/resume (AC6): stopping/restarting the server on the same
//!    address never resets the replica's applied position.
//! 5. `ResyncRequired` surfaced correctly over the wire.
//! 6. Config wiring: `[replication]` TOML `listen_addr`/`primary_addr` are
//!    auto-started by `with_unified_config`; `auth_token_env` resolution.
//!
//! Run with: `cargo test --test replication_tcp --features config-toml`

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aletheiadb::config::{AletheiaDBConfig, ReplicationConfig, WalConfigBuilder};
use aletheiadb::{
    AletheiaDB, DurabilityMode, FetchOutcome, NodeRole, PropertyMap, PropertyMapBuilder,
    ReplicationOptions, ReplicationServer, ReplicationSource, TcpSource,
};
use serial_test::serial;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn props(name: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// A durable primary bound to a known WAL directory (so resync tests can
/// reach into it directly), `Synchronous` durability so every commit is
/// fsynced before the call returns (the feed only ever sees durable data).
fn build_durable_primary() -> (tempfile::TempDir, AletheiaDB, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let wal_dir = dir.path().join("wal");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.clone())
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create primary");
    (dir, db, wal_dir)
}

/// A durable primary with a tiny segment size, so a handful of writes force
/// several sealed segments (used by the resync test).
fn build_durable_primary_small_segments() -> (tempfile::TempDir, AletheiaDB, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let wal_dir = dir.path().join("wal");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.clone())
                .durability_mode(DurabilityMode::Synchronous)
                .segment_size(1024)
                .expect("valid segment size")
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create primary");
    (dir, db, wal_dir)
}

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

fn loopback_addr(addr: SocketAddr) -> String {
    format!("127.0.0.1:{}", addr.port())
}

/// Every `.log.meta`-carrying (sealed) segment id under `wal_dir`.
fn sealed_segment_ids(wal_dir: &Path) -> Vec<u64> {
    let mut ids: Vec<u64> = std::fs::read_dir(wal_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".log.meta"))
                        .and_then(|id| id.parse::<u64>().ok())
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids
}

/// Delete both the `.log` and `.log.meta` files for segment `id` under
/// `wal_dir`, simulating retention/truncation having removed it.
fn delete_segment(wal_dir: &Path, id: u64) {
    let base = format!("{id:06}");
    let _ = std::fs::remove_file(wal_dir.join(format!("{base}.log")));
    let _ = std::fs::remove_file(wal_dir.join(format!("{base}.log.meta")));
}

// ---------------------------------------------------------------------------
// 1. Handshake
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn wrong_token_never_reaches_streaming_correct_token_streams() {
    let (_dir, primary, _wal_dir) = build_durable_primary();
    let primary = Arc::new(primary);
    primary
        .create_node("Person", props("seed"))
        .expect("seed node");

    let handle =
        ReplicationServer::start(Arc::clone(&primary), "127.0.0.1:0", "right-token".into())
            .expect("start server");
    let addr = loopback_addr(handle.local_addr());

    // Wrong token: the applier must never reach `streaming`.
    let bad_replica = AletheiaDB::new().expect("create replica");
    bad_replica
        .start_replication(
            Box::new(TcpSource::new(addr.clone(), "wrong-token")),
            fast_opts(),
        )
        .expect("start replication");

    // Give it several poll/retry cycles to (repeatedly, harmlessly) fail.
    std::thread::sleep(Duration::from_millis(400));
    let progress = bad_replica.replication_progress().expect("applier running");
    assert_ne!(progress.state, "streaming", "wrong token must never stream");
    assert_eq!(progress.last_applied_lsn, 0);
    assert!(
        progress
            .last_error
            .as_ref()
            .is_some_and(|e| e.to_lowercase().contains("auth")
                || e.to_lowercase().contains("token")
                || e.to_lowercase().contains("fetch failed")),
        "expected an auth-shaped error, got: {:?}",
        progress.last_error
    );

    // Correct token: streams and converges.
    let good_replica = AletheiaDB::new().expect("create replica");
    good_replica
        .start_replication(Box::new(TcpSource::new(addr, "right-token")), fast_opts())
        .expect("start replication");

    assert!(wait_until(Duration::from_secs(10), || good_replica
        .node_count()
        == primary.node_count()));
    assert_eq!(
        good_replica.replication_progress().expect("progress").state,
        "streaming"
    );
}

// ---------------------------------------------------------------------------
// 2. TCP streaming parity
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn tcp_streaming_parity_over_mixed_transaction_workload() {
    let (_dir, primary, _wal_dir) = build_durable_primary();
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
    for &id in &node_ids[0..3] {
        primary
            .update_node_with_valid_time(id, props("updated"), None)
            .expect("update node");
    }

    let handle = ReplicationServer::start(Arc::clone(&primary), "127.0.0.1:0", "tok".into())
        .expect("start server");
    let addr = loopback_addr(handle.local_addr());

    let replica = AletheiaDB::new().expect("create replica");
    replica
        .start_replication(Box::new(TcpSource::new(addr, "tok")), fast_opts())
        .expect("start replication");

    assert!(
        wait_until(Duration::from_secs(10), || {
            replica.node_count() == primary.node_count()
                && replica.edge_count() == primary.edge_count()
        }),
        "replica must converge to the primary's node/edge counts (primary: {}/{}, replica: {}/{})",
        primary.node_count(),
        primary.edge_count(),
        replica.node_count(),
        replica.edge_count(),
    );

    let primary_name = primary
        .get_node(node_ids[0])
        .expect("primary node")
        .properties
        .get("name")
        .map(|v| format!("{v:?}"));
    assert!(wait_until(Duration::from_secs(10), || {
        replica
            .get_node(node_ids[0])
            .ok()
            .and_then(|n| n.properties.get("name").map(|v| format!("{v:?}")))
            == primary_name
    }));
}

// ---------------------------------------------------------------------------
// 3. Snapshot bootstrap over TCP
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn snapshot_bootstrap_over_tcp_then_continued_streaming() {
    let (_dir, primary, _wal_dir) = build_durable_primary();
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

    let handle = ReplicationServer::start(Arc::clone(&primary), "127.0.0.1:0", "snap-tok".into())
        .expect("start server");
    let addr = loopback_addr(handle.local_addr());

    let replica_dir = tempfile::tempdir().expect("replica tempdir");
    let source = Box::new(TcpSource::new(addr, "snap-tok"));
    let replica = AletheiaDB::bootstrap_replica(source, replica_dir.path(), fast_opts())
        .expect("bootstrap replica over tcp");

    assert_eq!(replica.node_count(), primary.node_count());
    assert_eq!(replica.edge_count(), primary.edge_count());
    assert!(replica.get_node(alice).is_ok());

    // Continued streaming after bootstrap.
    let carol = primary
        .create_node("Person", props("Carol"))
        .expect("create carol");
    assert!(wait_until(Duration::from_secs(10), || replica
        .get_node(carol)
        .is_ok()));
}

// ---------------------------------------------------------------------------
// 4. Reconnect / resume (AC6)
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn reconnect_after_server_restart_resumes_without_resync() {
    let (_dir, primary, _wal_dir) = build_durable_primary();
    let primary = Arc::new(primary);

    for i in 0..5 {
        primary
            .create_node("Person", props(&format!("before-{i}")))
            .expect("create node");
    }

    let token = "reconnect-tok".to_string();
    let handle = ReplicationServer::start(Arc::clone(&primary), "127.0.0.1:0", token.clone())
        .expect("start server");
    let bound_addr = handle.local_addr();
    let addr = loopback_addr(bound_addr);

    let replica = AletheiaDB::new().expect("create replica");
    replica
        .start_replication(
            Box::new(TcpSource::new(addr.clone(), token.clone())),
            fast_opts(),
        )
        .expect("start replication");

    assert!(wait_until(Duration::from_secs(10), || replica.node_count()
        == primary.node_count()));
    let before_stop = replica
        .replication_progress()
        .expect("progress")
        .last_applied_lsn;
    assert!(before_stop > 0);

    // Stop the server: the replica must notice (state -> connecting) rather
    // than silently keep "succeeding" against a stale connection.
    handle.shutdown();
    drop(handle);

    assert!(wait_until(Duration::from_secs(10), || {
        replica
            .replication_progress()
            .map(|p| p.state == "connecting")
            .unwrap_or(false)
    }));

    // Sample the applied LSN repeatedly across the outage + restart window:
    // it must never decrease (no reset to LSN 1).
    let mut samples = vec![before_stop];
    let sampler_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < sampler_deadline {
        samples.push(
            replica
                .replication_progress()
                .expect("progress")
                .last_applied_lsn,
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // More writes on the primary while the server is down -- must not be
    // visible until the restarted server streams them.
    let post_restart_node = primary
        .create_node("Person", props("post-restart"))
        .expect("create post-restart node");

    // Rebind on the SAME address. Freeing the OS port can take a moment.
    let mut restarted = None;
    let bind_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < bind_deadline && restarted.is_none() {
        match ReplicationServer::start(Arc::clone(&primary), &addr, token.clone()) {
            Ok(h) => restarted = Some(h),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let _restarted_handle = restarted.expect("server rebinds on the same address within 5s");

    // Keep sampling through reconnection too.
    let resample_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < resample_deadline {
        samples.push(
            replica
                .replication_progress()
                .expect("progress")
                .last_applied_lsn,
        );
        if replica.get_node(post_restart_node).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        replica.get_node(post_restart_node).is_ok(),
        "replica must converge again after the server restarts"
    );
    assert_ne!(
        replica.replication_progress().expect("progress").state,
        "resync_required",
        "the primary never truncated anything; no resync should be needed"
    );

    for pair in samples.windows(2) {
        assert!(
            pair[1] >= pair[0],
            "last_applied_lsn must never regress across a reconnect: {:?}",
            samples
        );
    }
}

// ---------------------------------------------------------------------------
// 5. ResyncRequired over TCP
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn resync_required_is_surfaced_correctly_over_tcp() {
    let (_dir, primary, wal_dir) = build_durable_primary_small_segments();
    let primary = Arc::new(primary);

    // Force several segment rotations with a tiny segment_size.
    for i in 0..80 {
        primary
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("filler-{i}-{}", "x".repeat(64)))
                    .build(),
            )
            .expect("create node");
    }

    let sealed = sealed_segment_ids(&wal_dir);
    assert!(
        sealed.len() >= 2,
        "expected at least 2 sealed segments to simulate truncation, got {}: {:?}",
        sealed.len(),
        sealed
    );

    // Delete the OLDEST sealed segment -- simulating retention/truncation
    // having removed the WAL history a fresh (from_lsn=1) replica needs.
    delete_segment(&wal_dir, sealed[0]);

    let handle = ReplicationServer::start(Arc::clone(&primary), "127.0.0.1:0", "resync-tok".into())
        .expect("start server");
    let addr = loopback_addr(handle.local_addr());

    let replica = AletheiaDB::new().expect("create replica");
    replica
        .start_replication(Box::new(TcpSource::new(addr, "resync-tok")), fast_opts())
        .expect("start replication");

    assert!(wait_until(Duration::from_secs(10), || replica
        .replication_progress()
        .map(|p| p.state == "resync_required")
        .unwrap_or(false)));

    let progress = replica.replication_progress().expect("progress");
    assert_eq!(progress.state, "resync_required");
    assert_eq!(progress.last_applied_lsn, 0);

    // Progress stays frozen.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        replica
            .replication_progress()
            .expect("progress")
            .last_applied_lsn,
        0
    );
}

// ---------------------------------------------------------------------------
// 6. Config wiring
// ---------------------------------------------------------------------------

#[test]
#[cfg(feature = "config-toml")]
#[serial]
fn config_listen_addr_auto_starts_a_listening_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let toml = format!(
        r#"
[wal]
wal_dir = "{wal_dir}"

[replication]
listen_addr = "127.0.0.1:0"
auth_token = "toml-token"
"#,
        // Escape backslashes so Windows temp paths survive TOML basic-string
        // escaping (`\U` would otherwise start a unicode escape).
        wal_dir = dir
            .path()
            .join("wal")
            .display()
            .to_string()
            .replace('\\', "\\\\")
    );

    let config = AletheiaDBConfig::from_toml_str(&toml).expect("parse [replication] toml");
    assert_eq!(
        config.replication.listen_addr.as_deref(),
        Some("127.0.0.1:0")
    );

    let db = AletheiaDB::with_unified_config(config).expect("start db");
    let addr = db
        .replication_server_addr()
        .expect("listen server auto-started");

    // A raw connect + real handshake proves the server accepts connections
    // and authenticates -- no need for a full replica/applier here.
    let mut source = TcpSource::new(loopback_addr(addr), "toml-token");
    let outcome = source
        .fetch_entries(1, 10)
        .expect("handshake + fetch succeeds against the auto-started server");
    match outcome {
        FetchOutcome::Entries { entries, .. } => {
            assert!(entries.is_empty(), "fresh WAL has no entries yet")
        }
        FetchOutcome::ResyncRequired { .. } => {
            panic!("a fresh WAL must never report resync_required")
        }
    }
}

#[test]
#[serial]
fn config_primary_addr_auto_starts_replication_and_converges() {
    let (_dir, primary, _wal_dir) = build_durable_primary();
    let primary = Arc::new(primary);
    let seed = primary
        .create_node("Person", props("seed"))
        .expect("seed node");

    let token = "primary-addr-tok".to_string();
    let handle = ReplicationServer::start(Arc::clone(&primary), "127.0.0.1:0", token.clone())
        .expect("start server");
    let addr = loopback_addr(handle.local_addr());

    let replica_config = AletheiaDBConfig::builder()
        .replication(
            ReplicationConfig::builder()
                .primary_addr(addr)
                .auth_token(token)
                .poll_interval_ms(15)
                .build(),
        )
        .build();
    let replica = AletheiaDB::with_unified_config(replica_config).expect("start replica db");

    assert_eq!(replica.node_role(), NodeRole::Replica);
    assert!(wait_until(Duration::from_secs(10), || replica
        .get_node(seed)
        .is_ok()));
}

#[test]
#[serial]
fn config_auth_token_env_resolves_token_for_listen_server() {
    let var = "ALETHEIADB_TEST_REPLICATION_TOKEN_3355C";
    // SAFETY: env var mutation is process-global; `#[serial]` prevents any
    // other test in THIS binary from racing it, and the name is unique to
    // this test so it cannot collide with any other test binary's own env.
    unsafe {
        std::env::set_var(var, "env-resolved-token");
    }

    let config = AletheiaDBConfig::builder()
        .replication(
            ReplicationConfig::builder()
                .listen_addr("127.0.0.1:0")
                .auth_token_env(var)
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("start db");
    let addr = db
        .replication_server_addr()
        .expect("listen server auto-started");

    let mut source = TcpSource::new(loopback_addr(addr), "env-resolved-token");
    let outcome = source
        .fetch_entries(1, 10)
        .expect("handshake succeeds with the env-resolved token");
    assert!(matches!(outcome, FetchOutcome::Entries { .. }));

    unsafe {
        std::env::remove_var(var);
    }
}

#[test]
#[serial]
fn config_missing_token_fails_startup_fast() {
    let config = AletheiaDBConfig::builder()
        .replication(
            ReplicationConfig::builder()
                .listen_addr("127.0.0.1:0")
                .build(),
        )
        .build();

    let err = AletheiaDB::with_unified_config(config)
        .expect_err("listen_addr with no token must fail startup, not silently accept anonymous");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("token"),
        "error should mention the missing token, got: {msg}"
    );
}
