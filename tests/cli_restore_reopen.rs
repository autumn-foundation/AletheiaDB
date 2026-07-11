#![cfg(test)]

//! Regression test for `restore_to_data_dir` index-persistence layout.
//!
//! A directory produced by `AletheiaDB::restore_to_data_dir` (the durable
//! restore path used by the `aletheia restore <backup.albk>` CLI) must be
//! reopenable via the canonical durable entry point
//! `AletheiaDB::open` / `open_from_env` (which build their config through
//! `durable_config_for_data_dir`).
//!
//! Before the fix, `restore_to_data_dir` materialised the restored index
//! artifacts at `data_dir/indexes/...` while `durable_config_for_data_dir`
//! reads them from `data_dir/indexes/indexes/...` (the manager appends its own
//! `indexes/` under the configured base). The depth mismatch meant a reopen via
//! `open` found no manifest and came up empty, silently losing the restored
//! graph.
//!
//! We deliberately use single-key string properties (no vector embeddings) to
//! avoid the separate, known GLOBAL_INTERNER-mismatch flake.

use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use serial_test::serial;
use tempfile::TempDir;

/// A `.albk` backup restored into a fresh data dir must be reopenable through
/// the canonical `AletheiaDB::open` path with the full graph intact.
#[test]
#[serial]
fn restore_to_data_dir_is_reopenable_via_open() {
    // 1. Build a small source graph (string-only properties).
    let (backup_path, alice_id, bob_id, _tmp_src) = {
        let db = AletheiaDB::new().unwrap();

        let alice_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let bob_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(alice_id, bob_id, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);

        let tmp_src = TempDir::new().unwrap();
        let backup_path = tmp_src.path().join("graph.albk");
        db.backup(&backup_path).unwrap();

        // Drop the source DB before restoring: materialize_to_dir clears and
        // repopulates the process-global interner, so no live DB may share it.
        drop(db);

        (backup_path, alice_id, bob_id, tmp_src)
    };

    // 2. Restore into a fresh, empty data dir (the CLI `restore` path).
    let tmp_dst = TempDir::new().unwrap();
    let data_dir = tmp_dst.path().join("restored_db");

    {
        let restored = AletheiaDB::restore_to_data_dir(&backup_path, &data_dir).unwrap();
        assert_eq!(restored.node_count(), 2, "restore must reconstruct nodes");
        assert_eq!(restored.edge_count(), 1, "restore must reconstruct edges");
        // Drop the restored handle to flush/persist final state before reopening.
    }

    // 3. Reopen the restored dir through the canonical durable entry point.
    //    This is what `open_from_env`/the CLI reopen uses.
    let reopened = AletheiaDB::open(&data_dir).unwrap();

    assert_eq!(
        reopened.node_count(),
        2,
        "reopened DB must see the restored nodes (layout must match durable_config_for_data_dir)"
    );
    assert_eq!(
        reopened.edge_count(),
        1,
        "reopened DB must see the restored edge"
    );

    let alice = reopened.get_node(alice_id).unwrap();
    assert_eq!(
        alice.get_property("name").and_then(|v| v.as_str()),
        Some("Alice"),
        "Alice's restored property must survive reopen"
    );
    let bob = reopened.get_node(bob_id).unwrap();
    assert_eq!(
        bob.get_property("name").and_then(|v| v.as_str()),
        Some("Bob"),
        "Bob's restored property must survive reopen"
    );

    let outgoing = reopened.get_outgoing_edges(alice_id);
    assert_eq!(outgoing.len(), 1, "restored KNOWS edge must survive reopen");
}
