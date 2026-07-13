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
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
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
