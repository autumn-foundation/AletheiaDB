//! Regression tests for the process-global `GLOBAL_INTERNER.clear()` that used
//! to live in `materialize_to_dir` (`src/storage/backup/mod.rs`).
//!
//! Root cause: `AletheiaDB::restore()` (and the other `materialize_to_dir`
//! callers) cleared the PROCESS-GLOBAL string interner and re-interned only the
//! backup's strings from id 0. Any OTHER `AletheiaDB` live in the same process
//! then held dangling label / property-key ids, so the next WAL serialize raised
//! `"Label N not found in interner - data corruption detected"`
//! (`src/storage/wal/serialization.rs`) — or, worse, a SILENT wrong-label read.
//!
//! These tests are deliberately NOT `#[serial]`: the whole point is that a
//! restore in one place must not corrupt a live DB elsewhere in the same
//! process. Before the fix they fail (either the corruption error or a
//! wrong-label read); after the fix they pass.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aletheiadb::{AletheiaDB, GLOBAL_INTERNER, PropertyMapBuilder, PropertyValue};
use tempfile::TempDir;

/// Resolve an interned label id back to its string.
fn label_of(node: &aletheiadb::Node) -> String {
    GLOBAL_INTERNER
        .resolve_with(node.label, |s| s.to_string())
        .unwrap_or_default()
}

/// Read the `name` string property.
fn name_of(node: &aletheiadb::Node) -> Option<String> {
    match node.get_property("name") {
        Some(PropertyValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Build a self-contained backup artifact on disk (using a temp DB that is then
/// dropped) whose vocabulary differs from the live DB, and return its path plus
/// the owning tempdir (kept alive by the caller).
fn make_backup_with_vocab(tmp: &TempDir, labels: &[&str]) -> std::path::PathBuf {
    let src = AletheiaDB::new().unwrap();
    for (i, label) in labels.iter().enumerate() {
        src.create_node(
            label,
            PropertyMapBuilder::new()
                .insert("name", format!("{label}-{i}").as_str())
                .insert("k_backup_only", 1i64)
                .build(),
        )
        .unwrap();
    }
    let path = tmp.path().join("vocab.albk");
    src.backup(&path).unwrap();
    // Drop the source: its data lives entirely in the artifact now.
    drop(src);
    path
}

// ============================================================================
// Test 1 — the decisive concurrent-restore corruption regression.
// ============================================================================

/// A background thread hammers `AletheiaDB::restore()` while a separate, live
/// `AletheiaDB` in the same process creates and reads nodes. Under the buggy
/// `GLOBAL_INTERNER.clear()` this either raises a "not found in interner /
/// data corruption" error on the live DB's writes, or the live DB reads back
/// wrong labels. After the fix: zero errors, every label/property reads back
/// correctly.
#[test]
fn concurrent_restore_does_not_corrupt_live_db() {
    let tmp = TempDir::new().unwrap();
    // Backup vocabulary intentionally disjoint from the live DB's, so a
    // clear()+repopulate reassigns the live DB's ids to the WRONG strings.
    let backup_path = make_backup_with_vocab(
        &tmp,
        &["BackupCat", "BackupDog", "BackupBird", "BackupFish"],
    );

    let live = AletheiaDB::new().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let restore_path = backup_path.clone();
    let stop_restore = Arc::clone(&stop);
    let restorer = std::thread::spawn(move || {
        for _ in 0..40 {
            // Each restore materializes into a fresh temp dir and reopens it.
            let restored = AletheiaDB::restore(&restore_path).expect("restore failed");
            // Touch it so the restored data is exercised.
            let _ = restored.get_nodes_by_label("BackupCat");
            drop(restored);
            if stop_restore.load(Ordering::Relaxed) {
                break;
            }
        }
    });

    // Live workload concurrent with the restore storm.
    let mut created: Vec<(aletheiadb::NodeId, String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for i in 0..400usize {
        let label = "LivePerson";
        let name = format!("live-{i}");
        match live.create_node(
            label,
            PropertyMapBuilder::new()
                .insert("name", name.as_str())
                .insert("live_key", i as i64)
                .build(),
        ) {
            Ok(id) => {
                // Immediate read-back must resolve to the correct label + name.
                match live.get_node(id) {
                    Ok(node) => {
                        let got_label = label_of(&node);
                        let got_name = name_of(&node);
                        if got_label != label {
                            errors.push(format!(
                                "live node {id:?}: label read back as {got_label:?}, expected {label:?}"
                            ));
                        }
                        if got_name.as_deref() != Some(name.as_str()) {
                            errors.push(format!(
                                "live node {id:?}: name read back as {got_name:?}, expected {name:?}"
                            ));
                        }
                        created.push((id, label.to_string(), name));
                    }
                    Err(e) => errors.push(format!("get_node({id:?}) failed: {e}")),
                }
            }
            Err(e) => errors.push(format!("create_node #{i} failed: {e}")),
        }
    }

    stop.store(true, Ordering::Relaxed);
    restorer.join().expect("restore thread panicked");

    // Final re-verification of EVERY live node: under the bug the interner ends
    // up holding the last backup's vocabulary, so earlier live labels resolve to
    // the wrong string here even without a live-time race.
    for (id, expect_label, expect_name) in &created {
        match live.get_node(*id) {
            Ok(node) => {
                let got_label = label_of(&node);
                if &got_label != expect_label {
                    errors.push(format!(
                        "final: live node {id:?} label {got_label:?} != {expect_label:?}"
                    ));
                }
                if name_of(&node).as_deref() != Some(expect_name.as_str()) {
                    errors.push(format!(
                        "final: live node {id:?} name mismatch (expected {expect_name:?})"
                    ));
                }
            }
            Err(e) => errors.push(format!("final get_node({id:?}) failed: {e}")),
        }
    }

    assert!(
        errors.is_empty(),
        "concurrent restore corrupted the live DB's interner ({} errors); first few: {:?}",
        errors.len(),
        &errors[..errors.len().min(5)]
    );
}

// ============================================================================
// Test 2 — restore into a live process round-trip (both DBs correct).
// ============================================================================

/// With DB-A live and holding interned labels, `restore()` a backup into DB-B.
/// Afterward BOTH DBs must read their correct labels/properties: DB-A's ids must
/// not be corrupted by the restore, and DB-B's restored data must be correct.
#[test]
fn restore_into_live_process_preserves_both_dbs() {
    // DB-A: live, holds label "Alpha".
    let db_a = AletheiaDB::new().unwrap();
    let a_id = db_a
        .create_node(
            "Alpha",
            PropertyMapBuilder::new().insert("name", "a-one").build(),
        )
        .unwrap();

    // A backup with a disjoint vocabulary ("Beta"), created out-of-band.
    let tmp = TempDir::new().unwrap();
    let backup_path = make_backup_with_vocab(&tmp, &["Beta", "Gamma"]);

    // Restore DB-B into the same process while DB-A is alive.
    let db_b = AletheiaDB::restore(&backup_path).unwrap();

    // DB-A must be untouched.
    let a_node = db_a.get_node(a_id).unwrap();
    assert_eq!(
        label_of(&a_node),
        "Alpha",
        "DB-A label corrupted by restore"
    );
    assert_eq!(
        name_of(&a_node).as_deref(),
        Some("a-one"),
        "DB-A property corrupted by restore"
    );

    // DB-A must still accept writes and read them back correctly.
    let a_id2 = db_a
        .create_node(
            "Alpha",
            PropertyMapBuilder::new().insert("name", "a-two").build(),
        )
        .expect("DB-A write after restore failed");
    let a_node2 = db_a.get_node(a_id2).unwrap();
    assert_eq!(label_of(&a_node2), "Alpha");
    assert_eq!(name_of(&a_node2).as_deref(), Some("a-two"));

    // DB-B's restored data must be correct: a "Beta" node with name "Beta-0".
    let beta_nodes = db_b.get_nodes_by_label("Beta");
    assert_eq!(beta_nodes.len(), 1, "DB-B lost its restored Beta node");
    let beta = &beta_nodes[0];
    assert_eq!(label_of(beta), "Beta");
    assert_eq!(name_of(beta).as_deref(), Some("Beta-0"));

    // And DB-A's second node is still resolvable after touching DB-B.
    let a_node_again = db_a.get_node(a_id).unwrap();
    assert_eq!(label_of(&a_node_again), "Alpha");
}

// ============================================================================
// Test 4 (sanity) — normal single-DB restore round-trip still restores data.
// ============================================================================

/// A plain `restore()` with no concurrent DB must still round-trip data. Guards
/// against a fix that "isolates" by dropping data.
#[test]
fn single_db_restore_roundtrip_still_correct() {
    let tmp = TempDir::new().unwrap();
    let src = AletheiaDB::new().unwrap();
    let n1 = src
        .create_node(
            "Widget",
            PropertyMapBuilder::new()
                .insert("name", "w1")
                .insert("qty", 7i64)
                .build(),
        )
        .unwrap();
    let n2 = src
        .create_node(
            "Widget",
            PropertyMapBuilder::new().insert("name", "w2").build(),
        )
        .unwrap();
    src.create_edge(
        n1,
        n2,
        "LINKS",
        PropertyMapBuilder::new().insert("w", 1i64).build(),
    )
    .unwrap();
    let path = tmp.path().join("single.albk");
    src.backup(&path).unwrap();
    drop(src);

    let restored = AletheiaDB::restore(&path).unwrap();
    let r1 = restored.get_node(n1).unwrap();
    assert_eq!(label_of(&r1), "Widget");
    assert_eq!(name_of(&r1).as_deref(), Some("w1"));
    assert_eq!(
        r1.get_property("qty"),
        Some(&PropertyValue::Int(7)),
        "restored property value wrong"
    );
    let r2 = restored.get_node(n2).unwrap();
    assert_eq!(name_of(&r2).as_deref(), Some("w2"));
}

// ============================================================================
// Test 4b (sanity) — durable restore_to_data_dir round-trip + reopen.
// ============================================================================

/// `restore_to_data_dir` must materialize a durable dir whose labels reopen
/// correctly, with no dependence on a global interner clear.
#[test]
fn restore_to_data_dir_roundtrip_reopens_correct() {
    let tmp = TempDir::new().unwrap();
    let src = AletheiaDB::new().unwrap();
    let id = src
        .create_node(
            "Durable",
            PropertyMapBuilder::new().insert("name", "d1").build(),
        )
        .unwrap();
    let path = tmp.path().join("durable.albk");
    src.backup(&path).unwrap();
    drop(src);

    let data_dir = tmp.path().join("data");
    let db = AletheiaDB::restore_to_data_dir(&path, &data_dir).unwrap();
    let node = db.get_node(id).unwrap();
    assert_eq!(label_of(&node), "Durable");
    assert_eq!(name_of(&node).as_deref(), Some("d1"));
    drop(db);

    // Reopen the durable dir through the canonical path and re-verify.
    let reopened =
        AletheiaDB::with_unified_config(aletheiadb::config::durable_config_for_data_dir(&data_dir))
            .unwrap();
    let node2 = reopened.get_node(id).unwrap();
    assert_eq!(label_of(&node2), "Durable");
    assert_eq!(name_of(&node2).as_deref(), Some("d1"));
}
