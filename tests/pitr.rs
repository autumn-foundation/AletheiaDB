#![cfg(test)]

//! Integration tests for point-in-time restore (PITR, Issue #3374).
//!
//! PITR replays an archived WAL chain over a base `.albk` backup to an exact
//! transaction-time coordinate. These tests exercise the headline round-trip
//! equivalence property plus the boundary, error, read-only-input, provenance,
//! constraint, dry-run, and CLI-reopen behaviors.
//!
//! All tests are `#[serial]` because `restore` clears and repopulates the
//! process-global string interner (`materialize_to_dir`), so no two restores —
//! and no live source DB — may share the interner concurrently.

use std::path::{Path, PathBuf};

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::error::Error;
use aletheiadb::core::id::NodeId;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::storage::backup::BackupError;
use aletheiadb::{AletheiaDB, DurabilityMode, PitrTarget, PropertyMapBuilder, Timestamp};
use serial_test::serial;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// A WAL-only (no index persistence) source config with an isolated, durable
/// WAL directory we can archive and read back.
fn source_config(wal_dir: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.to_path_buf())
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .build()
}

/// Copy the flat set of `*.log` WAL segment files from `src` to a fresh `dst`.
fn copy_wal_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            std::fs::copy(&path, dst.join(entry.file_name())).unwrap();
        }
    }
}

/// A built PITR scenario: a base backup plus an archived WAL tail, with the
/// coordinates needed to target mid-stream transactions.
struct Scenario {
    _tmp: TempDir,
    albk: PathBuf,
    archive: PathBuf,
    source_lsn: u64,
    /// Pre-backup node ids (always present in the base).
    pre_ids: Vec<NodeId>,
    /// Post-backup `(id, commit_ts)` in commit order.
    post: Vec<(NodeId, Timestamp)>,
}

/// Build a scenario with `num_pre` pre-backup and `num_post` post-backup nodes,
/// all using a fixed `Person`/`name`/`phase` vocabulary interned before the
/// backup (so the WAL's interner ids resolve after restore).
fn build_scenario(num_pre: usize, num_post: usize) -> Scenario {
    let tmp = TempDir::new().unwrap();
    let wal = tmp.path().join("wal");
    let db = AletheiaDB::with_unified_config(source_config(&wal)).unwrap();

    let mut pre_ids = Vec::new();
    for i in 0..num_pre {
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("pre-{i}"))
                    .insert("phase", "pre")
                    .build(),
            )
            .unwrap();
        pre_ids.push(id);
    }

    let albk = tmp.path().join("base.albk");
    let source_lsn = db.backup(&albk).unwrap().source_lsn;

    let mut post = Vec::new();
    for i in 0..num_post {
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("post-{i}"))
                    .insert("phase", "post")
                    .build(),
            )
            .unwrap();
        let ts = db.get_node(id).unwrap().metadata.commit_timestamp.unwrap();
        post.push((id, ts));
    }

    let archive = tmp.path().join("archive");
    copy_wal_dir(&wal, &archive);

    // Drop the source DB before any restore: materialize_to_dir clears the
    // process-global interner, which no live DB may share.
    drop(db);

    Scenario {
        _tmp: tmp,
        albk,
        archive,
        source_lsn,
        pre_ids,
        post,
    }
}

fn name_of(db: &AletheiaDB, id: NodeId) -> Option<String> {
    db.get_node(id).ok().and_then(|n| {
        n.get_property("name")
            .and_then(|v| v.as_str().map(String::from))
    })
}

// ============================================================================
// Headline: round-trip observational equivalence
// ============================================================================

#[test]
#[serial]
fn pitr_roundtrip_observational_equivalence() {
    let s = build_scenario(2, 6);
    // Target the commit of post index 2 → post 0,1,2 kept; 3,4,5 dropped.
    let k = 2usize;
    let target_ts = s.post[k].1;

    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("restored");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(target_ts),
        &data_dir,
    )
    .unwrap();

    // 0 missing, 0 extra: exactly pre + post[0..=k].
    assert_eq!(restored.node_count(), s.pre_ids.len() + (k + 1));
    for id in &s.pre_ids {
        assert!(restored.get_node(*id).is_ok(), "pre node must survive");
    }
    for (i, (id, _)) in s.post.iter().enumerate() {
        if i <= k {
            assert!(
                restored.get_node(*id).is_ok(),
                "post-{i} committed at-or-before target must be present"
            );
        } else {
            assert!(
                restored.get_node(*id).is_err(),
                "post-{i} committed after target must be absent"
            );
        }
    }

    // Reference DB: an independent database that simply stopped writing at the
    // target coordinate. Build it AFTER restore (shares the repopulated
    // interner vocabulary) and compare observationally.
    let ref_tmp = TempDir::new().unwrap();
    let reference =
        AletheiaDB::with_unified_config(source_config(&ref_tmp.path().join("wal"))).unwrap();
    let mut ref_pre = Vec::new();
    for i in 0..s.pre_ids.len() {
        ref_pre.push(
            reference
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("name", format!("pre-{i}"))
                        .insert("phase", "pre")
                        .build(),
                )
                .unwrap(),
        );
    }
    let mut ref_post = Vec::new();
    for i in 0..=k {
        ref_post.push(
            reference
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("name", format!("post-{i}"))
                        .insert("phase", "post")
                        .build(),
                )
                .unwrap(),
        );
    }

    assert_eq!(
        restored.node_count(),
        reference.node_count(),
        "restored and reference must have identical current-state size"
    );

    // Current-state name sets must match (0 missing / 0 extra by content).
    let names = |db: &AletheiaDB, ids: &[NodeId]| -> Vec<String> {
        let mut v: Vec<String> = ids.iter().filter_map(|id| name_of(db, *id)).collect();
        v.sort();
        v
    };
    let mut restored_ids: Vec<NodeId> = s.pre_ids.clone();
    restored_ids.extend(s.post[..=k].iter().map(|(id, _)| *id));
    let mut reference_ids = ref_pre.clone();
    reference_ids.extend(ref_post.iter().copied());
    assert_eq!(
        names(&restored, &restored_ids),
        names(&reference, &reference_ids),
        "current-state node names must be observationally equivalent"
    );

    // Randomized-ish AS OF (transaction-time) probes: at every kept post
    // commit coordinate, the restored DB must resolve each kept node exactly as
    // its current state (the versions are unchanged after that commit).
    for probe in 0..=k {
        let probe_ts = s.post[probe].1;
        for (i, (id, _)) in s.post.iter().enumerate().take(probe + 1) {
            let at = restored.get_node_at_transaction_time(*id, probe_ts);
            assert!(
                at.is_ok(),
                "post-{i} must be visible AS OF the probe commit {probe}"
            );
        }
    }
}

// ============================================================================
// Boundary cases
// ============================================================================

#[test]
#[serial]
fn pitr_exact_target_at_a_commit() {
    let s = build_scenario(1, 4);
    let k = 1usize;
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(s.post[k].1),
        &data_dir,
    )
    .unwrap();
    assert!(
        restored.get_node(s.post[k].0).is_ok(),
        "node at exact target present"
    );
    assert!(
        restored.get_node(s.post[k + 1].0).is_err(),
        "next node absent"
    );
    assert_eq!(restored.node_count(), s.pre_ids.len() + (k + 1));
}

#[test]
#[serial]
fn pitr_target_equal_source_lsn_is_base_only() {
    let s = build_scenario(2, 3);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    // source_lsn is the next-to-allocate LSN at backup; no post-backup band
    // has a CommitTx LSN <= source_lsn, so the prefix is empty (base only).
    let restored = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::Lsn(s.source_lsn),
        &data_dir,
    )
    .unwrap();
    assert_eq!(restored.node_count(), s.pre_ids.len(), "base-only restore");
    for (id, _) in &s.post {
        assert!(
            restored.get_node(*id).is_err(),
            "no post node in base-only restore"
        );
    }
}

#[test]
#[serial]
fn pitr_target_at_latest_is_full_replay() {
    // A target AT the latest archived commit is in-window and replays every
    // post-backup transaction (the "restore to the tail" coordinate).
    let s = build_scenario(1, 5);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let latest_ts = s.post.last().unwrap().1;
    let restored = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(latest_ts),
        &data_dir,
    )
    .unwrap();
    assert_eq!(restored.node_count(), s.pre_ids.len() + s.post.len());
    for (id, _) in &s.post {
        assert!(
            restored.get_node(*id).is_ok(),
            "every post node present in full replay to the tail"
        );
    }
}

#[test]
#[serial]
fn pitr_target_above_window_errors() {
    // F2: an explicit target strictly ABOVE the archived WAL tail is an error
    // (not a silent full replay). The window is named in the structured error.
    let s = build_scenario(2, 3);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let far_future = Timestamp::from(i64::MAX / 2);
    let err = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(far_future),
        &data_dir,
    )
    .unwrap_err();
    match err {
        Error::Backup(BackupError::TargetOutsideWindow {
            requested, latest, ..
        }) => {
            assert!(!requested.is_empty());
            assert!(!latest.is_empty(), "the achievable tail must be named");
        }
        other => panic!("expected TargetOutsideWindow, got {other:?}"),
    }
    // The failed restore must not have materialized the target directory.
    assert!(
        !data_dir.join("indexes").exists(),
        "an above-window target must not materialize the data dir"
    );
}

// ============================================================================
// Errors and read-only-input guarantees
// ============================================================================

#[test]
#[serial]
fn pitr_target_outside_window_errors() {
    let s = build_scenario(2, 3);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    // An LSN below source_lsn cannot be reconstructed from base + forward WAL.
    let err = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::Lsn(s.source_lsn - 1),
        &data_dir,
    )
    .unwrap_err();
    match err {
        Error::Backup(BackupError::TargetOutsideWindow {
            requested,
            earliest,
            latest,
        }) => {
            assert!(requested.contains(&(s.source_lsn - 1).to_string()));
            assert!(earliest.contains(&format!("lsn={}", s.source_lsn)));
            assert!(!latest.is_empty());
        }
        other => panic!("expected TargetOutsideWindow, got {other:?}"),
    }
    // The failed restore must not have created the target directory.
    assert!(
        !data_dir.join("indexes").exists(),
        "an out-of-window target must not materialize the data dir"
    );
}

#[test]
#[serial]
fn pitr_non_empty_target_dir_fails_cleanly() {
    let s = build_scenario(1, 2);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    // Pre-populate the index root so check_target_empty refuses. The manager
    // appends its own `indexes/`, so the manifest it checks for lives at
    // `data_dir/indexes/indexes/manifest.idx`.
    let manifest_dir = data_dir.join("indexes").join("indexes");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(manifest_dir.join("manifest.idx"), b"fake").unwrap();
    let err = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(s.post[0].1),
        &data_dir,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Backup(BackupError::TargetNotEmpty)));
}

#[test]
#[serial]
fn pitr_inputs_are_read_only() {
    let s = build_scenario(1, 3);
    // Snapshot input bytes + mtimes before the restore.
    let albk_before = std::fs::read(&s.albk).unwrap();
    let archive_before: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&s.archive)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();

    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(s.post[1].1),
        &data_dir,
    )
    .unwrap();

    assert_eq!(
        std::fs::read(&s.albk).unwrap(),
        albk_before,
        "albk unchanged"
    );
    for (p, before) in &archive_before {
        assert_eq!(
            &std::fs::read(p).unwrap(),
            before,
            "archive segment unchanged"
        );
    }
}

// ============================================================================
// Subsystems: provenance, constraints, reopen
// ============================================================================

#[test]
#[serial]
fn pitr_preserves_provenance() {
    let tmp = TempDir::new().unwrap();
    let wal = tmp.path().join("wal");
    let db = AletheiaDB::with_unified_config(source_config(&wal)).unwrap();
    // Establish vocabulary + take a base backup with one node.
    let _seed = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("title", "seed").build(),
        )
        .unwrap();
    let source_lsn = db.backup(&tmp.path().join("base.albk")).unwrap().source_lsn;
    let _ = source_lsn;

    // Post-backup: a node carrying provenance.
    let prov = Provenance::builder()
        .source("hr-system")
        .confidence(0.95)
        .build()
        .unwrap();
    let provd = db
        .create_node_with_options(
            "Doc",
            PropertyMapBuilder::new().insert("title", "provd").build(),
            aletheiadb::api::transaction::WriteRequestOptions::new().with_provenance(prov),
        )
        .unwrap();
    let target_ts = db
        .get_node(provd)
        .unwrap()
        .metadata
        .commit_timestamp
        .unwrap();

    let archive = tmp.path().join("archive");
    copy_wal_dir(&wal, &archive);
    drop(db);

    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &tmp.path().join("base.albk"),
        &archive,
        PitrTarget::AsOf(target_ts),
        &data_dir,
    )
    .unwrap();

    let recovered = restored.get_node_provenance(provd).unwrap();
    let recovered = recovered.expect("provenance must survive PITR");
    assert_eq!(recovered.source(), Some("hr-system"));
}

#[test]
#[serial]
fn pitr_enforces_constraint_declared_before_target() {
    let tmp = TempDir::new().unwrap();
    let wal = tmp.path().join("wal");
    let db = AletheiaDB::with_unified_config(source_config(&wal)).unwrap();
    // Declare a unique constraint, then back up (declaration is below source_lsn).
    db.unique_constraint("User", "email").enable().unwrap();
    let seed = db
        .create_node(
            "User",
            PropertyMapBuilder::new().insert("email", "a@x.com").build(),
        )
        .unwrap();
    let source_lsn = db.backup(&tmp.path().join("base.albk")).unwrap().source_lsn;
    let _ = (seed, source_lsn);

    // Post-backup node under the same constraint.
    let post = db
        .create_node(
            "User",
            PropertyMapBuilder::new().insert("email", "b@x.com").build(),
        )
        .unwrap();
    let target_ts = db
        .get_node(post)
        .unwrap()
        .metadata
        .commit_timestamp
        .unwrap();

    let archive = tmp.path().join("archive");
    copy_wal_dir(&wal, &archive);
    drop(db);

    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &tmp.path().join("base.albk"),
        &archive,
        PitrTarget::AsOf(target_ts),
        &data_dir,
    )
    .unwrap();

    // The constraint declared before the target must be enforced in the
    // restored DB (a duplicate email is rejected).
    let dup = restored.create_node(
        "User",
        PropertyMapBuilder::new().insert("email", "a@x.com").build(),
    );
    assert!(
        dup.is_err(),
        "unique constraint must be enforced after PITR"
    );

    // And it must survive a reopen through the canonical durable path.
    drop(restored);
    let reopened = AletheiaDB::open(&data_dir).unwrap();
    let dup2 = reopened.create_node(
        "User",
        PropertyMapBuilder::new().insert("email", "b@x.com").build(),
    );
    assert!(
        dup2.is_err(),
        "unique constraint must survive reopen after PITR"
    );
}

#[test]
#[serial]
fn pitr_restored_dir_reopens_with_target_state() {
    let s = build_scenario(2, 5);
    let k = 1usize;
    let target_ts = s.post[k].1;
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    {
        let restored = AletheiaDB::restore_to_data_dir_at(
            &s.albk,
            &s.archive,
            PitrTarget::AsOf(target_ts),
            &data_dir,
        )
        .unwrap();
        assert_eq!(restored.node_count(), s.pre_ids.len() + (k + 1));
    }
    // Reopen must load exactly the persisted target state.
    let reopened = AletheiaDB::open(&data_dir).unwrap();
    assert_eq!(
        reopened.node_count(),
        s.pre_ids.len() + (k + 1),
        "reopen must reflect the persisted target state"
    );
    assert!(reopened.get_node(s.post[k].0).is_ok());
    assert!(reopened.get_node(s.post[k + 1].0).is_err());
}

// ============================================================================
// Dry-run / inspection
// ============================================================================

#[test]
#[serial]
fn pitr_inspect_reports_counts_and_window_without_side_effects() {
    let s = build_scenario(2, 6);
    let k = 2usize;
    let plan =
        AletheiaDB::inspect_pitr(&s.albk, &s.archive, Some(PitrTarget::AsOf(s.post[k].1))).unwrap();
    assert_eq!(
        plan.transactions_applied,
        (k + 1) as u64,
        "applied = kept post txns"
    );
    assert_eq!(
        plan.transactions_discarded,
        (s.post.len() - (k + 1)) as u64,
        "discarded = post txns after target"
    );
    assert_eq!(plan.earliest.lsn, s.source_lsn);
    assert_eq!(
        plan.resolved_stop.as_ref().unwrap().timestamp_micros,
        s.post[k].1.wallclock()
    );

    // No target: reports the full window, discards nothing.
    let full = AletheiaDB::inspect_pitr(&s.albk, &s.archive, None).unwrap();
    assert_eq!(full.transactions_applied, s.post.len() as u64);
    assert_eq!(full.transactions_discarded, 0);

    // Inspection must not create anything on disk.
    let ghost = TempDir::new().unwrap();
    let ghost_dir = ghost.path().join("never");
    let _ =
        AletheiaDB::inspect_pitr(&s.albk, &s.archive, Some(PitrTarget::Lsn(s.source_lsn))).unwrap();
    assert!(!ghost_dir.exists(), "inspect must not create a target dir");
}

// ============================================================================
// CLI reopen
// ============================================================================

#[test]
#[serial]
fn cli_pitr_restore_reopens_with_target_state() {
    let s = build_scenario(2, 5);
    let k = 2usize;
    let target_micros = s.post[k].1.wallclock();

    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");

    let bin = env!("CARGO_BIN_EXE_aletheia");
    let output = std::process::Command::new(bin)
        .args([
            "restore",
            s.albk.to_str().unwrap(),
            "--wal-archive",
            s.archive.to_str().unwrap(),
            "--as-of",
            &target_micros.to_string(),
        ])
        .env("ALETHEIADB_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI PITR restore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The produced dir reopens and reflects the target state.
    let reopened = AletheiaDB::open(&data_dir).unwrap();
    assert_eq!(reopened.node_count(), s.pre_ids.len() + (k + 1));
    assert!(reopened.get_node(s.post[k].0).is_ok());
    assert!(reopened.get_node(s.post[k + 1].0).is_err());
}

#[test]
#[serial]
fn cli_pitr_dry_run_prints_plan_json() {
    let s = build_scenario(1, 4);
    let bin = env!("CARGO_BIN_EXE_aletheia");
    let output = std::process::Command::new(bin)
        .args([
            "restore",
            s.albk.to_str().unwrap(),
            "--wal-archive",
            s.archive.to_str().unwrap(),
            "--lsn",
            &s.source_lsn.to_string(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        json["transactions_applied"], 0,
        "base-only target applies nothing"
    );
    assert_eq!(json["transactions_discarded"], s.post.len());
    assert!(json["earliest"]["lsn"].as_u64().unwrap() == s.source_lsn);
}

#[test]
#[serial]
fn cli_pitr_restore_latest_replays_full_tail() {
    // F2: `restore --wal-archive <dir>` with no target flag (equivalently
    // `--latest`) performs a FULL replay to the archived tail.
    let s = build_scenario(2, 4);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");

    let bin = env!("CARGO_BIN_EXE_aletheia");
    let output = std::process::Command::new(bin)
        .args([
            "restore",
            s.albk.to_str().unwrap(),
            "--wal-archive",
            s.archive.to_str().unwrap(),
            "--latest",
        ])
        .env("ALETHEIADB_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI --latest restore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let reopened = AletheiaDB::open(&data_dir).unwrap();
    assert_eq!(
        reopened.node_count(),
        s.pre_ids.len() + s.post.len(),
        "full replay to the tail includes every post-backup node"
    );
    for (id, _) in &s.post {
        assert!(reopened.get_node(*id).is_ok());
    }
}

// ============================================================================
// F1: interner vocabulary-change guard
// ============================================================================

#[test]
#[serial]
fn pitr_window_crossing_vocabulary_change_errors() {
    // A PITR whose window crosses a post-backup vocabulary change (a brand-new
    // label whose interner id is absent from the base backup) must FAIL with a
    // structured error rather than silently mislabel/drop the replayed node.
    // A PITR to a target BEFORE the vocabulary change still succeeds.
    let tmp = TempDir::new().unwrap();
    let wal = tmp.path().join("wal");
    let db = AletheiaDB::with_unified_config(source_config(&wal)).unwrap();

    // Base vocabulary: Person / name. Take the base backup.
    let pre = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "alice").build(),
        )
        .unwrap();
    db.backup(&tmp.path().join("base.albk")).unwrap();

    // Post-backup, still using the base vocabulary: a target before any change.
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "bob").build(),
        )
        .unwrap();
    let ts_before_change = db.get_node(bob).unwrap().metadata.commit_timestamp.unwrap();

    // Post-backup vocabulary change: a BRAND-NEW label not present in the base
    // interner (a unique string never interned before this write).
    let mgr = db
        .create_node(
            "MgrNovelLabel3374",
            PropertyMapBuilder::new().insert("name", "boss").build(),
        )
        .unwrap();
    let ts_after_change = db.get_node(mgr).unwrap().metadata.commit_timestamp.unwrap();

    let archive = tmp.path().join("archive");
    copy_wal_dir(&wal, &archive);
    drop(db);

    // (a) Target at/after the vocabulary change → structured refusal.
    let dst_fail = TempDir::new().unwrap();
    let data_dir_fail = dst_fail.path().join("db");
    let err = AletheiaDB::restore_to_data_dir_at(
        &tmp.path().join("base.albk"),
        &archive,
        PitrTarget::AsOf(ts_after_change),
        &data_dir_fail,
    )
    .unwrap_err();
    match err {
        Error::Backup(BackupError::WindowCrossesVocabularyChange {
            first_unresolved_id,
            restored_interner_count,
        }) => {
            assert!(
                first_unresolved_id >= restored_interner_count,
                "the unresolved id must be outside the base interner"
            );
        }
        other => panic!("expected WindowCrossesVocabularyChange, got {other:?}"),
    }
    assert!(
        !data_dir_fail.join("indexes").exists(),
        "a vocabulary-crossing restore must not materialize the data dir"
    );

    // (b) Target BEFORE the vocabulary change → clean success (Manager absent).
    let dst_ok = TempDir::new().unwrap();
    let data_dir_ok = dst_ok.path().join("db");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &tmp.path().join("base.albk"),
        &archive,
        PitrTarget::AsOf(ts_before_change),
        &data_dir_ok,
    )
    .unwrap();
    assert_eq!(
        restored.node_count(),
        2,
        "pre (alice) + bob, before the vocabulary change"
    );
    assert!(restored.get_node(pre).is_ok());
    assert!(restored.get_node(bob).is_ok());
    assert!(
        restored.get_node(mgr).is_err(),
        "the new-label node is after the target and must be absent"
    );
}

// ============================================================================
// F4: failed restore after materialize leaves a clean target
// ============================================================================

#[test]
#[serial]
fn pitr_post_materialize_failure_leaves_clean_target() {
    // If a step AFTER the base manifest is committed fails, the target
    // `indexes/` must be cleaned up so a retry is not blocked by a half-restored
    // directory. We inject a deterministic post-materialize failure by planting
    // a regular FILE where the WAL directory must be created, so opening the
    // base-state database fails only after `materialize_to_dir` has run.
    let s = build_scenario(1, 3);
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");

    // Plant a file at data_dir/wal so the durable open cannot create the WAL dir.
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("wal"), b"not a directory").unwrap();

    let result = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(s.post[1].1),
        &data_dir,
    );
    assert!(
        result.is_err(),
        "restore must fail when the WAL dir cannot be created"
    );

    // The target index root must be cleaned up (no half-restored base left
    // behind), so a subsequent restore into a fresh dir would not be blocked.
    assert!(
        !data_dir.join("indexes").exists(),
        "a failed post-materialize restore must leave the target indexes/ clean"
    );
}

// ============================================================================
// F3: large exact-prefix fidelity (AC success metric)
// ============================================================================

#[test]
#[serial]
fn pitr_large_stream_restores_exact_prefix() {
    // AC success metric names a 10,000-transaction fixture; the synchronous WAL
    // fsync-per-commit source makes 10K writes heavy for this disk/time ceiling,
    // so the fixture is SCALED to a few thousand transactions. The property under
    // test is unchanged: PITR to a mid-stream target restores EXACTLY the prefix
    // committed at-or-before the target — 0 missing, 0 extra — versus the
    // transactions that were written.
    const SCALED_POST_TX: usize = 3_000; // scaled from the 10K AC metric (see above)
    let s = build_scenario(4, SCALED_POST_TX);

    // Target the midpoint transaction: post[0..=k] must be present, the rest
    // absent.
    let k = SCALED_POST_TX / 2;
    let target_ts = s.post[k].1;

    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &s.albk,
        &s.archive,
        PitrTarget::AsOf(target_ts),
        &data_dir,
    )
    .unwrap();

    // Exact size: pre + the kept prefix, no more, no less.
    assert_eq!(
        restored.node_count(),
        s.pre_ids.len() + (k + 1),
        "restored size must equal pre + kept prefix exactly"
    );
    // Every pre node survives.
    for id in &s.pre_ids {
        assert!(restored.get_node(*id).is_ok(), "pre node must survive");
    }
    // Exact membership: present iff committed at-or-before the target.
    for (i, (id, _)) in s.post.iter().enumerate() {
        if i <= k {
            assert!(
                restored.get_node(*id).is_ok(),
                "post-{i} at-or-before target must be present (0 missing)"
            );
        } else {
            assert!(
                restored.get_node(*id).is_err(),
                "post-{i} after target must be absent (0 extra)"
            );
        }
    }
}

// ============================================================================
// #3387: PITR HLC seed folds restored CLOSED transaction-time ends
// ============================================================================

#[test]
#[serial]
fn pitr_hlc_seed_stays_monotonic_over_restored_closed_tx_end() {
    // Guards the Issue #3387 invariant through the PITR path: when a post-backup
    // transaction SUPERSEDES an earlier version, that earlier version's
    // transaction-time interval is CLOSED at the superseding transaction's commit
    // timestamp. The restored database's HLC seed must fold that closed tx end in
    // (via the shared startup seed helper), so the first write on the restored DB
    // is stamped strictly ABOVE it — never regressing to a stale timestamp that
    // would break transaction-time monotonicity.
    let tmp = TempDir::new().unwrap();
    let wal = tmp.path().join("wal");
    let db = AletheiaDB::with_unified_config(source_config(&wal)).unwrap();

    // Pre-backup: create node A (v1) using the base vocabulary, then back up.
    let a = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "alice")
                .insert("phase", "v1")
                .build(),
        )
        .unwrap();
    db.backup(&tmp.path().join("base.albk")).unwrap();

    // Post-backup: UPDATE A (v2). This supersedes v1, closing v1's
    // transaction-time interval at v2's commit timestamp — the closed tx end.
    db.update_node_with_valid_time(
        a,
        PropertyMapBuilder::new()
            .insert("name", "alice")
            .insert("phase", "v2")
            .build(),
        None,
    )
    .unwrap();
    // v2's commit timestamp equals the restored closed tx end of v1 (replay
    // reproduces the update op verbatim), so it is the coordinate the HLC seed
    // must clear.
    let closed_tx_end = db.get_node(a).unwrap().metadata.commit_timestamp.unwrap();

    let archive = tmp.path().join("archive");
    copy_wal_dir(&wal, &archive);
    drop(db);

    // Restore to a target that INCLUDES the superseding update, so the restored
    // history carries v1's closed transaction-time interval.
    let dst = TempDir::new().unwrap();
    let data_dir = dst.path().join("db");
    let restored = AletheiaDB::restore_to_data_dir_at(
        &tmp.path().join("base.albk"),
        &archive,
        PitrTarget::AsOf(closed_tx_end),
        &data_dir,
    )
    .unwrap();

    // A fresh write on the restored DB must be stamped strictly above the closed
    // tx end (HLC seed stayed monotonic — no regression to a stale timestamp).
    let fresh = restored
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "bob")
                .insert("phase", "post-restore")
                .build(),
        )
        .unwrap();
    let fresh_ts = restored
        .get_node(fresh)
        .unwrap()
        .metadata
        .commit_timestamp
        .unwrap();
    assert!(
        fresh_ts > closed_tx_end,
        "restored HLC seed must clear the restored closed tx end \
         (fresh {fresh_ts:?} must exceed closed end {closed_tx_end:?})"
    );
}
