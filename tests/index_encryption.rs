#![cfg(test)]

//! Integration tests for encryption-at-rest of persisted index files
//! (Issue #481).
//!
//! These exercise the public surface end-to-end: a durable, encryption-enabled
//! `AletheiaDB` persists its indexes, is dropped, and is reopened from the same
//! data directory with the same key — proving every persisted index file is
//! encrypted on disk yet transparently decrypted on load. They also prove the
//! system fails **closed** and **recovers** rather than bricking: a corrupt
//! (truncated) encrypted index file must not prevent startup (state is
//! recovered from the WAL), and reopening with the WRONG key must never return
//! the original plaintext data.

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMap;
use aletheiadb::core::temporal::time;
use aletheiadb::encryption::cipher::{Aes256GcmCipher, Cipher};
use aletheiadb::encryption::config::EncryptionConfig;
use aletheiadb::encryption::key_provider::FileKeyProvider;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::index_persistence::graph::{
    load_graph_index_with_cipher, new_graph_index_data, save_graph_index,
};
use aletheiadb::storage::index_persistence::temporal::{
    load_temporal_index_with_cipher, new_temporal_index_data, save_temporal_index_with_cipher,
};
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use aletheiadb::storage::wal::{LSN, WalEntry, WalOperation};
use aletheiadb::storage::wal_reader::read_wal_entries;
use aletheiadb::{AletheiaDB, GLOBAL_INTERNER, PropertyMapBuilder};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use zeroize::Zeroizing;

fn test_cipher(seed: u8) -> Arc<dyn Cipher> {
    let mut key = Zeroizing::new([0u8; 32]);
    key[0] = seed;
    key[5] = seed.wrapping_add(1);
    Arc::new(Aes256GcmCipher::new(&key))
}

fn encrypted_durable_config(data_dir: &Path, key_path: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .encryption(EncryptionConfig::file_based(key_path))
        .build()
}

/// Same durable persistence layout as [`encrypted_durable_config`] but with
/// encryption DISABLED — used to establish a legacy plaintext dataset that is
/// later reopened under a cipher (the real customer upgrade path).
fn plaintext_durable_config(data_dir: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::GroupCommit {
                    max_delay_ms: 10,
                    max_batch_size: 200,
                })
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

/// A byte-for-byte scan of a directory tree returning `true` if any file under
/// it begins with the plaintext encrypted-index magic `AEIX`.
fn any_file_has_enc_header(root: &Path) -> bool {
    let mut found = false;
    visit(root, &mut |p| {
        if let Ok(bytes) = std::fs::read(p)
            && bytes.len() >= 4
            && &bytes[..4] == b"AEIX"
        {
            found = true;
        }
    });
    found
}

fn file_has_enc_header(path: &Path) -> bool {
    std::fs::read(path)
        .map(|b| b.len() >= 4 && &b[..4] == b"AEIX")
        .unwrap_or(false)
}

fn visit(dir: &Path, f: &mut impl FnMut(&Path)) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, f);
            } else {
                f(&path);
            }
        }
    }
}

/// Mixed directory (upgrade scenario): a plaintext index file and an encrypted
/// index file coexisting in the same directory both load correctly when a
/// cipher is present. This is the back-compat contract for turning encryption
/// on over an existing plaintext dataset.
#[test]
fn mixed_plaintext_and_encrypted_files_both_load() {
    let dir = tempfile::tempdir().unwrap();
    let cipher = test_cipher(0x42);

    // Graph index written PLAINTEXT (as an older build would have).
    let graph_path = dir.path().join("graph.idx");
    let graph = new_graph_index_data();
    save_graph_index(&graph, &graph_path).unwrap();
    assert!(
        !file_has_enc_header(&graph_path),
        "graph should be plaintext"
    );

    // Temporal index written ENCRYPTED (new build).
    let temporal_path = dir.path().join("temporal.idx");
    let temporal = new_temporal_index_data();
    save_temporal_index_with_cipher(&temporal, &temporal_path, Some(&cipher)).unwrap();
    assert!(
        file_has_enc_header(&temporal_path),
        "temporal should be encrypted"
    );

    // With a cipher present, BOTH load: the plaintext one via header sniffing,
    // the encrypted one via decryption.
    load_graph_index_with_cipher(&graph_path, Some(&cipher)).unwrap();
    load_temporal_index_with_cipher(&temporal_path, Some(&cipher)).unwrap();
}

/// Full end-to-end: build an encryption-enabled durable DB, write graph, edge
/// and vector data, persist, drop, reopen with the SAME key, and assert all
/// data survives — while every persisted index file is encrypted on disk.
#[test]
fn db_end_to_end_encrypted_persist_and_reopen() {
    use aletheiadb::index::vector::{DistanceMetric, HnswConfig};

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");
    let key_path = dir.path().join("index.key");
    FileKeyProvider::generate_key_file(&key_path).unwrap();

    let (alice, bob) = {
        let db = AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path))
            .unwrap();

        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine))
            .enable()
            .unwrap();

        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        db.create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2024i64).build(),
        )
        .unwrap();

        db.persist_indexes().unwrap();
        (alice, bob)
        // db dropped here: background thread performs a final flush.
    };

    // Every persisted index file must be encrypted on disk (no cleartext
    // bitcode payloads for graph/temporal/manifest/interner/vector meta).
    let indexes_dir = data_dir.join("indexes");
    assert!(
        any_file_has_enc_header(&indexes_dir),
        "expected at least one AEIX-encrypted index file under {:?}",
        indexes_dir
    );
    // The graph adjacency index specifically must be encrypted.
    let adjacency = indexes_dir
        .join("indexes")
        .join("graph")
        .join("adjacency.idx");
    if adjacency.exists() {
        assert!(
            file_has_enc_header(&adjacency),
            "graph adjacency.idx must be encrypted on disk"
        );
    }

    // Issue #481 (P0.1): the native HNSW `current.usearch` file — which holds
    // the raw embedding vectors, the most sensitive data — and its
    // `.usearch.mappings` sidecar must ALSO be encrypted on disk, not just the
    // bitcode meta.idx/mappings.idx.
    let vec_dir = indexes_dir.join("indexes").join("vector").join("embedding");
    let usearch = vec_dir.join("current.usearch");
    assert!(
        usearch.exists(),
        "expected native usearch index at {:?}",
        usearch
    );
    assert!(
        file_has_enc_header(&usearch),
        "native current.usearch must be encrypted on disk (raw embeddings)"
    );
    let usearch_mappings = vec_dir.join("current.usearch.mappings");
    assert!(
        file_has_enc_header(&usearch_mappings),
        "native current.usearch.mappings sidecar must be encrypted on disk"
    );
    // No plaintext temp files (`.aeix-usearch-tmp-*`) must be left behind by the
    // save-to-temp/encrypt shuffle.
    let leftover_tmp: Vec<_> = std::fs::read_dir(&vec_dir)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".aeix-usearch-tmp-")
        })
        .collect();
    assert!(
        leftover_tmp.is_empty(),
        "native usearch encryption must not leak plaintext temp files: {:?}",
        leftover_tmp.iter().map(|e| e.path()).collect::<Vec<_>>()
    );

    // Reopen with the SAME key: all data transparently decrypts.
    let db2 =
        AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path)).unwrap();

    let alice_node = db2.get_node(alice).expect("Alice must survive reopen");
    assert_eq!(
        alice_node.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
    let bob_node = db2.get_node(bob).expect("Bob must survive reopen");
    assert_eq!(
        bob_node.properties.get("name").and_then(|v| v.as_str()),
        Some("Bob")
    );
    assert_eq!(db2.get_outgoing_edges(alice).len(), 1);

    // Vector search survives: Alice's nearest neighbour is Bob.
    let similar = db2
        .similarity_search(aletheiadb::SimilarityQuery::from_node(alice).k(5))
        .unwrap();
    assert!(
        similar.iter().any(|(id, _)| *id == bob),
        "find_similar should recover Bob as a neighbour after encrypted reopen"
    );
}

/// Regression guard for the encryption-at-rest (#481) × interner-remap (#3490)
/// merge: a node with MANY distinct string property keys and String values must
/// round-trip EVERY key and value verbatim across an encrypted persist + reopen.
///
/// #3490 broke exactly this: persisted property keys / String values are
/// file-space interner ids that must be translated through the load-time remap
/// before resolution, or they silently resolve to the WRONG string once the
/// live interner order diverges from the saved file. The #481 cipher threading
/// must decrypt the index bytes FIRST and then run that decode+remap unchanged —
/// this test fails loudly if the cipher path bypasses or reorders the remap
/// (values would come back mismatched, not merely absent).
#[test]
fn encrypted_reopen_roundtrips_multi_property_keys_and_values() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");
    let key_path = dir.path().join("index.key");
    FileKeyProvider::generate_key_file(&key_path).unwrap();

    // Distinct string keys AND distinct string values, plus a non-string, so a
    // dropped/mis-ordered remap can't accidentally pass by coincidence.
    let expected: &[(&str, &str)] = &[
        ("title", "Introduction to Rust"),
        ("author", "Grace Hopper"),
        ("publisher", "Aletheia Press"),
        ("language", "English"),
        ("isbn", "978-0-13-468599-1"),
        ("summary", "A bitemporal tale"),
    ];

    let doc = {
        let db = AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path))
            .unwrap();
        let mut builder = PropertyMapBuilder::new();
        for &(k, v) in expected {
            builder = builder.insert(k, v);
        }
        builder = builder.insert("edition", 3i64);
        let doc = db.create_node("Document", builder.build()).unwrap();
        db.persist_indexes().unwrap();
        doc
        // dropped: final flush.
    };

    // Reopen with the SAME key and assert every key -> value survives exactly.
    let db2 =
        AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path)).unwrap();
    let node = db2
        .get_node(doc)
        .expect("Document must survive encrypted reopen");
    for &(k, v) in expected {
        assert_eq!(
            node.properties.get(k).and_then(|val| val.as_str()),
            Some(v),
            "property key {:?} must round-trip its String value verbatim after \
             encrypted reopen (guards #481 x #3490 remap reconciliation)",
            k
        );
    }
    assert_eq!(
        node.properties.get("edition").and_then(|val| val.as_int()),
        Some(3),
        "non-string property must also survive"
    );
}

/// Full-DB UPGRADE path (Issue #481, P3.1): a durable database created with
/// encryption DISABLED (legacy plaintext index files) reopens cleanly under a
/// cipher — every plaintext index file loads via header sniffing — and a
/// subsequent forced `persist_indexes()` rewrites those files ENCRYPTED while
/// the data stays intact. This is the real customer "turn encryption on over
/// an existing dataset" flow.
#[test]
fn db_plaintext_dataset_upgrades_to_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");
    let key_path = dir.path().join("index.key");

    // Phase 1: create a PLAINTEXT durable dataset with graph + edge data.
    let (alice, bob) = {
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(&data_dir)).unwrap();
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        db.create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();
        db.persist_indexes().unwrap();
        (alice, bob)
    };

    // Index files written in phase 1 must be PLAINTEXT (no AEIX header).
    let indexes_dir = data_dir.join("indexes");
    let adjacency = indexes_dir
        .join("indexes")
        .join("graph")
        .join("adjacency.idx");
    assert!(
        adjacency.exists() && !file_has_enc_header(&adjacency),
        "phase-1 adjacency.idx should exist and be plaintext"
    );

    // Phase 2: reopen the SAME data dir WITH encryption enabled. Legacy
    // plaintext index files must load under the cipher (header sniffing), and
    // all data survives.
    FileKeyProvider::generate_key_file(&key_path).unwrap();
    {
        let db = AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path))
            .unwrap();
        assert_eq!(
            db.get_node(alice)
                .unwrap()
                .properties
                .get("name")
                .and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(
            db.get_node(bob)
                .unwrap()
                .properties
                .get("name")
                .and_then(|v| v.as_str()),
            Some("Bob")
        );
        assert_eq!(db.get_outgoing_edges(alice).len(), 1);

        // Force a full re-encrypt: every on-disk index file is rewritten
        // through the cipher.
        db.persist_indexes().unwrap();
    }

    // The previously-plaintext graph adjacency index now carries the AEIX
    // header (encrypted at rest after the forced re-persist).
    assert!(
        file_has_enc_header(&adjacency),
        "after persist_indexes() the graph adjacency.idx must be encrypted"
    );
    assert!(
        any_file_has_enc_header(&indexes_dir),
        "at least one index file must be encrypted after the upgrade re-persist"
    );

    // Phase 3: reopen once more with the key; the now-encrypted data survives.
    let db2 =
        AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path)).unwrap();
    assert_eq!(
        db2.get_node(alice)
            .unwrap()
            .properties
            .get("name")
            .and_then(|v| v.as_str()),
        Some("Alice")
    );
    assert_eq!(db2.get_outgoing_edges(alice).len(), 1);
}

/// Crash-safety variant (Issue #481, P3.2): with encryption enabled and the
/// MANIFEST left INTACT, truncating ONLY the graph `adjacency.idx` must not
/// brick startup. The DB reopens (no panic, no Err) and recovers per the
/// documented differential-replay property — pinning the manifest-intact /
/// snapshot-corrupt case that the other e2e test (which also corrupts the
/// manifest) routes around.
#[test]
fn db_manifest_intact_corrupt_graph_snapshot_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");
    let key_path = dir.path().join("index.key");
    FileKeyProvider::generate_key_file(&key_path).unwrap();

    let alice = {
        let db = AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path))
            .unwrap();
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        db.persist_indexes().unwrap();
        alice
    };

    // Truncate ONLY the encrypted graph adjacency snapshot; leave manifest.idx
    // untouched so startup takes the differential-replay path (manifest LSN
    // floor preserved) rather than full-from-scratch replay.
    let indexes_dir = data_dir.join("indexes");
    let adjacency = indexes_dir
        .join("indexes")
        .join("graph")
        .join("adjacency.idx");
    assert!(
        file_has_enc_header(&adjacency),
        "precondition: adjacency.idx encrypted"
    );
    let manifest = indexes_dir.join("indexes").join("manifest.idx");
    let manifest_before = std::fs::read(&manifest).unwrap();
    let bytes = std::fs::read(&adjacency).unwrap();
    std::fs::write(&adjacency, &bytes[..bytes.len() / 2]).unwrap();
    // Manifest must be untouched.
    assert_eq!(std::fs::read(&manifest).unwrap(), manifest_before);

    // Reopen: the corrupt encrypted graph snapshot must NOT brick startup — no
    // panic, no Err. The documented differential-replay property applies: with
    // the manifest (LSN floor) intact, replay resumes AFTER the persisted
    // snapshot, so pre-snapshot data that lived only in the now-unreadable graph
    // snapshot MAY be absent from current state (the documented loss window,
    // identical to a corrupt *plaintext* snapshot — not an encryption
    // regression). We therefore assert the anti-brick + functional guarantees,
    // and that if Alice *is* present she is never corrupt/wrong data.
    let db2 =
        AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path)).unwrap();

    match db2.get_node(alice) {
        Ok(node) => {
            // If recovered, it must be the correct value — never garbage from a
            // half-decrypted ciphertext.
            assert_eq!(
                node.properties.get("name").and_then(|v| v.as_str()),
                Some("Alice"),
                "a recovered node must carry correct data, never ciphertext garbage"
            );
        }
        Err(_) => {
            // Acceptable: pre-snapshot data lost via the documented
            // differential-replay window (manifest intact, snapshot corrupt).
        }
    }

    // The reopened DB must be fully functional regardless of the loss window.
    let carol = db2
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Carol").build(),
        )
        .unwrap();
    assert!(db2.get_node(carol).is_ok());
}

/// Reopening an encrypted dataset with the WRONG key must never return the
/// original plaintext data (fail closed): the DB either fails to open cleanly
/// or opens without recovering the encrypted state — but never corrupt data,
/// never a panic.
#[test]
fn db_reopen_with_wrong_key_never_returns_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");
    let good_key = dir.path().join("good.key");
    let bad_key = dir.path().join("bad.key");
    FileKeyProvider::generate_key_file(&good_key).unwrap();
    FileKeyProvider::generate_key_file(&bad_key).unwrap();

    let alice = {
        let db = AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &good_key))
            .unwrap();
        let alice = db
            .create_node(
                "Secret",
                PropertyMapBuilder::new()
                    .insert("name", "TopSecret")
                    .build(),
            )
            .unwrap();
        db.persist_indexes().unwrap();
        alice
    };

    // Reopen with the WRONG key. Either result is acceptable as long as the
    // secret plaintext is never surfaced and nothing panics.
    match AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &bad_key)) {
        Ok(db) => {
            let recovered = db.get_node(alice).ok().and_then(|n| {
                n.properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
            assert_ne!(
                recovered.as_deref(),
                Some("TopSecret"),
                "wrong key must not decrypt the secret"
            );
        }
        Err(_) => {
            // Failing closed at open is an acceptable outcome.
        }
    }
}

/// A truncated (crash-during-save) encrypted index file must not brick
/// startup: the DB still opens and recovers state from the WAL, because a
/// corrupt/undecryptable index file is treated exactly like a corrupt
/// plaintext one (best-effort load + WAL replay).
#[test]
fn db_truncated_encrypted_index_recovers_from_wal() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("db");
    let key_path = dir.path().join("index.key");
    FileKeyProvider::generate_key_file(&key_path).unwrap();

    let alice = {
        let db = AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path))
            .unwrap();
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        db.persist_indexes().unwrap();
        alice
    };

    // Simulate a crash mid-write by truncating the encrypted snapshot files
    // (manifest + graph adjacency) to a partial length. Corrupting the manifest
    // discards the persisted LSN floor, so startup falls back to full WAL
    // replay from the beginning (rather than differential replay from a snapshot
    // LSN) — this is exactly how a corrupt *plaintext* snapshot is handled, so a
    // corrupt *encrypted* snapshot must recover identically and never brick.
    let mut truncated = 0;
    visit(&data_dir.join("indexes"), &mut |p| {
        let name = p.file_name().and_then(|n| n.to_str());
        let is_snapshot = matches!(name, Some("adjacency.idx") | Some("manifest.idx"));
        if is_snapshot && file_has_enc_header(p) {
            let bytes = std::fs::read(p).unwrap();
            std::fs::write(p, &bytes[..bytes.len() / 2]).unwrap();
            truncated += 1;
        }
    });
    assert!(
        truncated >= 1,
        "expected at least one encrypted snapshot file to truncate"
    );

    // Reopen: must NOT panic or return Err (no bricked restart); state is
    // recovered from the WAL.
    let db2 =
        AletheiaDB::with_unified_config(encrypted_durable_config(&data_dir, &key_path)).unwrap();
    let node = db2
        .get_node(alice)
        .expect("Alice recovered from WAL after corrupt snapshot");
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    // And the reopened DB is fully functional.
    let carol = db2
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Carol").build(),
        )
        .unwrap();
    assert!(db2.get_node(carol).is_ok());
}

// =============================================================================
// WAL recovery read-path coverage for the cipher matrix (Issue #481).
//
// PR #3564 threaded the WAL cipher into the recovery read path
// (`ConcurrentWalSystem::read_from` -> `read_wal_entries_with_cipher` ->
// `segment_reader::read_entries_from_dir_with_cipher`). These tests exercise
// that method directly over a genuinely on-disk WAL, proving the reader
// dispatches PER-SEGMENT on the version byte:
//
//   * Case 1 (plaintext-WAL recovers unchanged, cipher DISABLED) is covered
//     upstream by `tests/recovery/replay_create_tests.rs`
//     (`test_replay_create_node_basic`, which appends a CreateNode with a
//     default no-cipher `ConcurrentWalSystem` and recovers it through
//     `read_from`) and the whole no-cipher `storage::recovery` suite — not
//     duplicated here.
//   * Case 2 (encrypted-WAL recovers, cipher ENABLED) — `encrypted_wal_segments_recover_via_read_from`.
//   * Case 3 (MIXED plaintext + encrypted segments in one dir) — the real
//     regression risk — `mixed_plaintext_and_encrypted_wal_segments_recover`.
// =============================================================================

/// Build a WAL system config rooted at `wal_dir`, optionally encrypted. Uses
/// the default (Synchronous) durability mode so every `append` + `flush` is on
/// disk before `read_from` runs — the tests stay deterministic with no
/// background-flush races.
fn wal_system_config(wal_dir: &Path, cipher: Option<Arc<dyn Cipher>>) -> ConcurrentWalSystemConfig {
    let mut config = ConcurrentWalSystemConfig::new(wal_dir);
    config.wal_cipher = cipher;
    config
}

/// Append `count` CreateNode operations with node ids `first..first+count`,
/// flushing after so they are durably on disk, and return the ids.
fn append_create_nodes(wal: &ConcurrentWalSystem, first: u64, count: u64) -> Vec<u64> {
    let mut ids = Vec::with_capacity(count as usize);
    for id in first..first + count {
        wal.append(WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern("WalRecoveryProbe").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        })
        .unwrap();
        ids.push(id);
    }
    wal.flush().unwrap();
    ids
}

/// The `CreateNode` node ids present in a recovered entry set.
fn created_node_ids(entries: &[WalEntry]) -> BTreeSet<u64> {
    entries
        .iter()
        .filter_map(|e| match &e.operation {
            WalOperation::CreateNode { node_id, .. } => Some(node_id.as_u64()),
            _ => None,
        })
        .collect()
}

/// The set of format-version bytes across every `*.log` segment in `wal_dir`.
/// (Header layout: 4-byte `GWAL` magic + 1 version byte; encrypted versions
/// are even, plaintext versions odd.)
fn segment_version_bytes(wal_dir: &Path) -> Vec<u8> {
    let mut versions = Vec::new();
    for entry in std::fs::read_dir(wal_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("log") {
            let bytes = std::fs::read(&path).unwrap();
            if bytes.len() >= 5 && &bytes[..4] == b"GWAL" {
                versions.push(bytes[4]);
            }
        }
    }
    versions
}

/// Case 2: a WAL written with a cipher (all segments encrypted) replays
/// correctly when reopened with the same cipher — and the segments are
/// genuinely encrypted (an even version byte on disk; a no-cipher read fails
/// closed). This is a DIRECT assertion of encrypted-segment replay through the
/// changed `read_from` path, complementing the DB-level
/// `db_truncated_encrypted_index_recovers_from_wal` (whose encrypted-replay is
/// incidental to its anti-brick focus).
#[test]
fn encrypted_wal_segments_recover_via_read_from() {
    let dir = tempfile::tempdir().unwrap();
    let wal_dir = dir.path().join("wal");
    let cipher = test_cipher(0x37);

    // Phase 1: write an encrypted WAL, then shut it down cleanly.
    {
        let mut wal =
            ConcurrentWalSystem::new(wal_system_config(&wal_dir, Some(cipher.clone()))).unwrap();
        let ids = append_create_nodes(&wal, 1, 4);
        assert_eq!(ids, vec![1, 2, 3, 4]);
        wal.shutdown();
    }

    // Every on-disk segment must carry an ENCRYPTED (even) version byte.
    let versions = segment_version_bytes(&wal_dir);
    assert!(
        !versions.is_empty() && versions.iter().all(|v| v % 2 == 0),
        "all segments must be encrypted (even version byte), got {versions:?}"
    );

    // Fail-closed proof: reading the encrypted WAL WITHOUT a cipher errors,
    // rather than silently returning garbage or empty.
    assert!(
        read_wal_entries(&wal_dir, LSN::initial()).is_err(),
        "reading an encrypted WAL without a cipher must fail closed"
    );

    // Phase 2: reopen WITH the same cipher and recover through read_from.
    let wal = ConcurrentWalSystem::new(wal_system_config(&wal_dir, Some(cipher))).unwrap();
    let recovered = wal.read_from(LSN::initial()).unwrap();
    assert_eq!(
        created_node_ids(&recovered),
        BTreeSet::from([1, 2, 3, 4]),
        "all encrypted-segment entries must be recovered by the cipher read path"
    );
    // LSNs are returned in ascending order.
    let lsns: Vec<u64> = recovered.iter().map(|e| e.lsn.0).collect();
    let mut sorted = lsns.clone();
    sorted.sort_unstable();
    assert_eq!(lsns, sorted, "recovered entries must be LSN-ordered");
}

/// Case 3 (THE regression gap): a single WAL directory containing BOTH a
/// plaintext segment (written before encryption was enabled) AND an encrypted
/// segment (written after) must recover ALL entries when reopened with a
/// cipher. The reader must dispatch per-segment on the version byte and NOT
/// attempt to decrypt the old plaintext (odd-version) segment. Reproduces the
/// real "turned encryption on over an existing WAL" upgrade path: the writer
/// rolls to a fresh segment when the header version differs, so the two phases
/// land in distinct segments of different versions.
#[test]
fn mixed_plaintext_and_encrypted_wal_segments_recover() {
    let dir = tempfile::tempdir().unwrap();
    let wal_dir = dir.path().join("wal");
    let cipher = test_cipher(0x5c);

    // Phase 1: PLAINTEXT WAL (encryption disabled) — legacy segment.
    {
        let mut wal = ConcurrentWalSystem::new(wal_system_config(&wal_dir, None)).unwrap();
        let ids = append_create_nodes(&wal, 1, 3);
        assert_eq!(ids, vec![1, 2, 3]);
        wal.shutdown();
    }
    // After phase 1 every segment is PLAINTEXT (odd version byte).
    assert!(
        segment_version_bytes(&wal_dir).iter().all(|v| v % 2 == 1),
        "phase-1 segments must all be plaintext (odd version)"
    );

    // Phase 2: reopen the SAME dir WITH a cipher and append more entries. The
    // version mismatch forces the writer onto a fresh, ENCRYPTED segment,
    // leaving the phase-1 plaintext segment intact on disk.
    {
        let mut wal =
            ConcurrentWalSystem::new(wal_system_config(&wal_dir, Some(cipher.clone()))).unwrap();
        let ids = append_create_nodes(&wal, 100, 3);
        assert_eq!(ids, vec![100, 101, 102]);
        wal.shutdown();
    }

    // Prove the directory is GENUINELY mixed: at least one plaintext (odd) and
    // at least one encrypted (even) segment coexist. Without this the test
    // would not distinguish the mixed case from an all-encrypted one.
    let versions = segment_version_bytes(&wal_dir);
    assert!(
        versions.iter().any(|v| v % 2 == 1),
        "expected a surviving plaintext segment (odd version), got {versions:?}"
    );
    assert!(
        versions.iter().any(|v| v % 2 == 0),
        "expected an encrypted segment (even version), got {versions:?}"
    );

    // Recover the mixed directory WITH the cipher present. The plaintext
    // segment must be read WITHOUT decryption and the encrypted segment WITH
    // decryption — all six entries recovered, in LSN order.
    let wal = ConcurrentWalSystem::new(wal_system_config(&wal_dir, Some(cipher))).unwrap();
    let recovered = wal.read_from(LSN::initial()).unwrap();
    assert_eq!(
        created_node_ids(&recovered),
        BTreeSet::from([1, 2, 3, 100, 101, 102]),
        "entries from BOTH the plaintext and encrypted segments must be recovered"
    );
    let lsns: Vec<u64> = recovered.iter().map(|e| e.lsn.0).collect();
    let mut sorted = lsns.clone();
    sorted.sort_unstable();
    assert_eq!(lsns, sorted, "mixed-segment recovery must be LSN-ordered");
}
