//! Tests for logging + honoring the tombstone/retraction `version_id` in the
//! WAL delete/retract payloads (Issue #3406).
//!
//! Background
//! ----------
//! The live write path pre-generates a tombstone/retraction `VersionId` for
//! every `DeleteNode`/`DeleteEdge`/`RetractNode`/`RetractEdge` from the shared
//! `version_id_gen`. Before this issue the WAL delete/retract payloads did NOT
//! carry that id, so crash recovery SYNTHESIZED a tombstone id from
//! `next_version_id` (max-seen + 1). Two consequences:
//!
//!   1. Recovered tombstone ids could DIFFER from the live ones -> version
//!      chains not bit-identical across a crash.
//!   2. A synthesized tombstone id could COLLIDE with a `version_id` carried by
//!      a later-logged `Update` entry -> both land under the same global
//!      `VersionId` map key and the later insert silently CLOBBERS the earlier
//!      -> history loss on recovery.
//!
//! The fix logs the tombstone/retraction `version_id` (WAL v9/v10) and makes
//! replay HONOR it (bumping `next_version_id` past it), with a synthesis
//! fallback for pre-v9 segments that never carried the field.
//!
//! These tests drive REAL on-disk WAL replay via `CheckpointManager::recover`
//! and inspect `HistoricalStorage` directly to assert exact version ids.

use aletheiadb::{
    AletheiaDB, GLOBAL_INTERNER,
    config::{AletheiaDBConfig, WalConfigBuilder},
    core::error::Result,
    core::{
        id::{EdgeId, NodeId, VersionId},
        property::PropertyMapBuilder,
        temporal::time,
    },
    storage::{
        checkpoint::{CheckpointConfig, CheckpointManager},
        index_persistence::PersistenceConfig,
        wal::{
            DurabilityMode, LSN, WalOperation,
            concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig},
        },
    },
};
use tempfile::TempDir;

/// Build a fresh raw WAL in a tempdir.
fn new_wal(temp_dir: &TempDir) -> Result<ConcurrentWalSystem> {
    let wal_dir = temp_dir.path().join("wal");
    ConcurrentWalSystem::new(ConcurrentWalSystemConfig::new(wal_dir))
}

/// Recover a WAL from scratch (full replay — no checkpoint/index snapshot to
/// short-circuit it) and return the reconstructed storages.
fn recover(
    temp_dir: &TempDir,
    wal: &ConcurrentWalSystem,
) -> Result<(
    aletheiadb::storage::current::CurrentStorage,
    aletheiadb::storage::historical::HistoricalStorage,
)> {
    let config = CheckpointConfig::with_data_dir(temp_dir.path().join("checkpoints"));
    let mut manager = CheckpointManager::new(config)?;
    let (current, historical, _lsn) = manager.recover(wal)?;
    Ok((current, historical))
}

fn person(name: &str) -> aletheiadb::core::property::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

// ---------------------------------------------------------------------------
// AC #1 + #2: replay HONORS the logged tombstone/retraction version_id, so the
// recovered head version id equals the id the live path logged (bit-identical).
// ---------------------------------------------------------------------------

#[test]
fn honor_logged_delete_node_version_id() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;
    let node_id = NodeId::new(1).unwrap();

    // A distinct, high tombstone id — one a synthesizing replay would NEVER
    // pick (synthesis would use max-seen + 1, i.e. a small value).
    let live_tombstone = VersionId::new(7777).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: person("Alice"),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
        version_id: Some(live_tombstone),
    })?;
    wal.flush()?;

    let (current, historical) = recover(&temp_dir, &wal)?;

    // Node is gone from current state, but its head historical version is the
    // EXACT logged tombstone id — not a synthesized one.
    assert!(current.get_node(node_id).is_err());
    assert_eq!(
        historical.get_current_node_version(node_id),
        Some(live_tombstone),
        "recovered tombstone head must equal the logged live version_id"
    );
    let v = historical
        .get_node_version(live_tombstone)
        .expect("tombstone version must exist under the logged id");
    assert_eq!(v.node_id, node_id);

    Ok(())
}

#[test]
fn honor_logged_delete_edge_version_id() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;
    let src = NodeId::new(1).unwrap();
    let tgt = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let live_tombstone = VersionId::new(8888).unwrap();

    for (id, name) in [(src, "Alice"), (tgt, "Bob")] {
        wal.append(WalOperation::CreateNode {
            node_id: id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: person(name),
            valid_from: time::now(),
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: src,
        target: tgt,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteEdge {
        edge_id,
        valid_from: time::now(),
        version_id: Some(live_tombstone),
    })?;
    wal.flush()?;

    let (current, historical) = recover(&temp_dir, &wal)?;

    assert!(current.get_edge(edge_id).is_err());
    assert_eq!(
        historical.get_current_edge_version(edge_id),
        Some(live_tombstone),
        "recovered edge tombstone head must equal the logged live version_id"
    );
    Ok(())
}

#[test]
fn honor_logged_retract_node_version_id() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;
    let node_id = NodeId::new(1).unwrap();
    let live_retraction = VersionId::new(9999).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: person("Alice"),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::RetractNode {
        node_id,
        valid_to: time::now(),
        version_id: Some(live_retraction),
    })?;
    wal.flush()?;

    let (_current, historical) = recover(&temp_dir, &wal)?;

    assert_eq!(
        historical.get_current_node_version(node_id),
        Some(live_retraction),
        "recovered retraction head must equal the logged live version_id"
    );
    Ok(())
}

#[test]
fn honor_logged_retract_edge_version_id() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;
    let src = NodeId::new(1).unwrap();
    let tgt = NodeId::new(2).unwrap();
    let edge_id = EdgeId::new(1).unwrap();
    let live_retraction = VersionId::new(4242).unwrap();

    for (id, name) in [(src, "Alice"), (tgt, "Bob")] {
        wal.append(WalOperation::CreateNode {
            node_id: id,
            label: GLOBAL_INTERNER.intern("Person").unwrap(),
            properties: person(name),
            valid_from: time::now(),
            provenance: None,
        })?;
    }
    wal.append(WalOperation::CreateEdge {
        edge_id,
        source: src,
        target: tgt,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMapBuilder::new().build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::RetractEdge {
        edge_id,
        valid_to: time::now(),
        version_id: Some(live_retraction),
    })?;
    wal.flush()?;

    let (_current, historical) = recover(&temp_dir, &wal)?;

    assert_eq!(
        historical.get_current_edge_version(edge_id),
        Some(live_retraction),
        "recovered edge retraction head must equal the logged live version_id"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// AC #3 (load-bearing): a synthesized tombstone id colliding with a
// later-logged Update's version_id must NOT clobber either version.
// ---------------------------------------------------------------------------

#[test]
fn collision_does_not_clobber_history() -> Result<()> {
    // Fresh recovery seeds next_version_id = 0, so:
    //   CreateNode(A) -> version 0   (next = 1)
    //   CreateNode(B) -> version 1   (next = 2)
    //   DeleteNode(A) -> pre-fix synth tombstone = 2 (next = 3)
    //   UpdateNode(B, version_id = 2) -> collides with A's synth tombstone!
    //
    // With the fix, DeleteNode(A) carries a distinct logged tombstone id
    // (500), so A's tombstone (500) and B's update (2) never share a key and
    // neither is lost.
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;

    let a = NodeId::new(1).unwrap();
    let b = NodeId::new(2).unwrap();

    // The real, unique live tombstone id for A's delete — deliberately high so
    // it differs from what synthesis (2) would pick.
    let a_tombstone = VersionId::new(500).unwrap();
    // B's update id — equal to the value a SYNTHESIZING replay would assign to
    // A's tombstone. This is the collision.
    let b_update = VersionId::new(2).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id: a,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: person("Alice"),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::CreateNode {
        node_id: b,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: person("Bob"),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteNode {
        node_id: a,
        valid_from: time::now(),
        version_id: Some(a_tombstone),
    })?;
    wal.append(WalOperation::UpdateNode {
        node_id: b,
        version_id: b_update,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 31_i64)
            .build(),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.flush()?;

    let (_current, historical) = recover(&temp_dir, &wal)?;

    // A's tombstone must live under its OWN logged id and belong to A.
    let a_v = historical
        .get_node_version(a_tombstone)
        .expect("A's tombstone must exist under its logged id (not clobbered)");
    assert_eq!(a_v.node_id, a, "A's tombstone id must map to node A");
    assert_eq!(
        historical.get_current_node_version(a),
        Some(a_tombstone),
        "A's head must be its logged tombstone"
    );

    // B's update must live under version id 2 and belong to B — proving the
    // key that synthesis would have collided on is B's, uncorrupted.
    let b_v = historical
        .get_node_version(b_update)
        .expect("B's update must exist");
    assert_eq!(b_v.node_id, b, "version id 2 must map to node B's update");
    assert_eq!(
        historical.get_current_node_version(b),
        Some(b_update),
        "B's head must be its update version"
    );

    // Nothing lost: A (create + tombstone) + B (create + update) = 4 versions.
    let stats = historical.stats();
    assert_eq!(
        stats.total_node_versions, 4,
        "no version may be clobbered: expected 4 node versions"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Idempotency: repeated replay yields the same (logged) tombstone id + chain.
// ---------------------------------------------------------------------------

#[test]
fn repeated_replay_is_stable() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;
    let node_id = NodeId::new(1).unwrap();
    let live_tombstone = VersionId::new(1234).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: person("Alice"),
        valid_from: time::now(),
        provenance: None,
    })?;
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
        version_id: Some(live_tombstone),
    })?;
    wal.flush()?;

    let (_c1, h1) = recover(&temp_dir, &wal)?;
    let (_c2, h2) = recover(&temp_dir, &wal)?;

    assert_eq!(h1.get_current_node_version(node_id), Some(live_tombstone));
    assert_eq!(
        h1.get_current_node_version(node_id),
        h2.get_current_node_version(node_id),
        "tombstone id must be stable across repeated replays"
    );
    assert_eq!(
        h1.stats().total_node_versions,
        h2.stats().total_node_versions,
        "version chain length must be stable across repeated replays"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// AC #4: back-compat — a segment WITHOUT a logged tombstone version_id (old
// format, simulated with `version_id: None`) still replays via synthesis
// without error.
// ---------------------------------------------------------------------------

#[test]
fn back_compat_synthesizes_when_version_id_absent() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal = new_wal(&temp_dir)?;
    let node_id = NodeId::new(1).unwrap();

    wal.append(WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: person("Alice"),
        valid_from: time::now(),
        provenance: None,
    })?;
    // `None` mirrors a pre-v9 segment that never carried the field.
    wal.append(WalOperation::DeleteNode {
        node_id,
        valid_from: time::now(),
        version_id: None,
    })?;
    wal.flush()?;

    let (current, historical) = recover(&temp_dir, &wal)?;

    // No error, node deleted, tombstone synthesized (head present), 2 versions.
    assert!(current.get_node(node_id).is_err());
    assert!(
        historical.get_current_node_version(node_id).is_some(),
        "a synthesized tombstone head must exist for a pre-v9 delete"
    );
    assert_eq!(historical.stats().total_node_versions, 2);
    Ok(())
}

// ---------------------------------------------------------------------------
// End-to-end: a REAL live commit logs the tombstone id it applied, and a
// crash + full WAL replay recovers that exact id (bit-identical chain).
// ---------------------------------------------------------------------------

#[test]
fn live_delete_tombstone_id_survives_crash() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    // Durable, WAL-only (no index snapshot), fsync-per-commit.
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.clone())
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..PersistenceConfig::default()
        })
        .build();

    let node_id = {
        let db = AletheiaDB::with_unified_config(config).expect("open db");
        let id = db
            .create_node("Person", person("Alice"))
            .expect("create_node");
        db.delete_node_with_valid_time(id, None)
            .expect("delete_node");
        // Hard crash: leak the handle so Drop never persists anything.
        std::mem::forget(db);
        id
    };

    // Reopen the WAL from disk and extract the LIVE tombstone id the commit
    // logged into the DeleteNode payload (Issue #3406).
    let wal = ConcurrentWalSystem::new(ConcurrentWalSystemConfig::new(wal_dir))?;
    let entries = wal.read_from(LSN::initial())?;
    let live_tombstone = entries
        .iter()
        .find_map(|e| match &e.operation {
            WalOperation::DeleteNode {
                node_id: n,
                version_id,
                ..
            } if *n == node_id => Some(*version_id),
            _ => None,
        })
        .expect("a DeleteNode entry for our node must be in the WAL")
        .expect("the live DeleteNode entry must carry a logged tombstone version_id");

    // Full replay recovers that exact id.
    let (current, historical) = recover(&temp_dir, &wal)?;
    assert!(current.get_node(node_id).is_err());
    assert_eq!(
        historical.get_current_node_version(node_id),
        Some(live_tombstone),
        "recovered tombstone head must equal the id the LIVE commit logged"
    );

    Ok(())
}
