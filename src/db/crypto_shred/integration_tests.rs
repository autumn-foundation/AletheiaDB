//! Crypto-shred **property-path integration** tests (Issue #3359, slice PR-1b).
//!
//! These exercise the live seal-at-write / unseal-at-read data path wired in
//! PR-1b: an end-to-end cold-restart round trip, the silent-bypass sentinel
//! byte-scan across every in-scope persisted tier, blast-radius isolation,
//! `__aletheia_ns` coexistence, HNSW exclusion of designated embeddings, the
//! MCP erased-marker shape, a no-deadlock concurrency check, and the
//! non-designated zero-regression guarantee.

use std::path::Path;
use std::sync::Arc;

use crate::config::{AletheiaDBConfig, WalConfigBuilder};
use crate::core::property::{PropertyMapBuilder, PropertyValue};
use crate::db::AletheiaDB;
use crate::encryption::config::EncryptionConfig;
use crate::index::vector::{DistanceMetric, HnswConfig};
use crate::storage::index_persistence::PersistenceConfig;

use super::DesignationTarget;

// ── helpers ────────────────────────────────────────────────────────

/// A distinctive plaintext sentinel that must NEVER appear in any persisted
/// artifact once its subject is sealed/erased.
const SENTINEL: &str = "SENTINEL_GDPR_a9f3c1e7b2d64051_PLAINTEXT_MUST_NOT_LEAK";

/// Build a persistent, encrypted config rooted at `root`.
fn enc_config(root: &Path) -> AletheiaDBConfig {
    let key_file = root.join("mek.key");
    if !key_file.exists() {
        crate::encryption::FileKeyProvider::generate_key_file(&key_file).unwrap();
    }
    AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().wal_dir(root.join("wal")).build())
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: root.join("data"),
            load_on_startup: true,
            ..Default::default()
        })
        .encryption(EncryptionConfig::file_based(&key_file))
        .build()
}

fn enc_db(root: &Path) -> AletheiaDB {
    AletheiaDB::with_unified_config(enc_config(root)).unwrap()
}

/// A distinctive embedding whose raw little-endian float bytes we also scan for.
fn sentinel_embedding() -> Vec<f32> {
    vec![0.111_111_f32, 0.222_222, 0.333_333, 0.444_444]
}

/// The raw LE byte encoding of an embedding, as it would appear in a plaintext
/// vector index / property store if it were ever persisted un-sealed.
fn embedding_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Recursively assert no persisted file under `dir` contains `needle`.
///
/// Reusable silent-bypass guard (AC5 for the in-scope hot tiers): after
/// designate + seal-at-write + erase, EVERY reachable artifact (WAL segments,
/// index-persistence files incl. usearch `.bin`, current-tier persisted files,
/// checkpoint) must be free of the designated plaintext. The MEK key file is
/// skipped (it is key material, never a data artifact, and never carries the
/// sentinel).
fn assert_no_designated_plaintext(dir: &Path, needle: &[u8]) {
    assert!(!needle.is_empty(), "needle must be non-empty");
    let mut stack = vec![dir.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(path) = stack.pop() {
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for e in entries.flatten() {
                    stack.push(e.path());
                }
            }
            continue;
        }
        // Skip the MEK key file — key material, not a data artifact.
        if path.file_name().is_some_and(|n| n == "mek.key") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        scanned += 1;
        let hit = bytes.windows(needle.len()).any(|w| w == needle);
        assert!(
            !hit,
            "designated plaintext leaked into persisted artifact: {}",
            path.display()
        );
    }
    assert!(scanned > 0, "scan found no files under {}", dir.display());
}

// ── E2E: seal at write, unseal at read, survive cold restart ───────

#[test]
fn e2e_seal_write_unseal_read_survives_cold_restart_then_erase() {
    let dir = tempfile::tempdir().unwrap();
    let embedding = sentinel_embedding();

    // Phase 1: designate a whole node + write the sentinel + a designated
    // embedding, so the persisted version is sealed at write time.
    let node_id;
    {
        let db = enc_db(dir.path());
        db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
            .unwrap();

        // Create a placeholder (no sentinel), capture its id, THEN designate and
        // replace so the sealed version carries the sentinel. This is robust to
        // whatever id the generator assigns.
        node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("stage", "placeholder")
                    .build(),
            )
            .unwrap();
        db.designate_subject(
            "subject-A",
            vec![DesignationTarget::WholeNode(node_id.as_u64())],
        )
        .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new()
                .insert("email", SENTINEL)
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

        // In-process read already returns plaintext (key present).
        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("email"),
            Some(&PropertyValue::from(SENTINEL.to_string())),
            "active-subject read must unseal to plaintext"
        );
        drop(db);
    }

    // Phase 2: reopen COLD — key still present, read returns plaintext.
    {
        let db = enc_db(dir.path());
        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("email"),
            Some(&PropertyValue::from(SENTINEL.to_string())),
            "cold reopen with key present must return plaintext"
        );
        // The embedding round-trips as plaintext through the read boundary too.
        assert!(
            node.properties
                .get("embedding")
                .and_then(|v| v.as_vector())
                .is_some(),
            "designated embedding must unseal back to a Vector on read"
        );
        drop(db);
    }

    // Phase 3: erase the subject, reopen COLD — read returns the erased marker,
    // NEVER the sentinel.
    {
        let db = enc_db(dir.path());
        db.erase_subject("subject-A").unwrap();
        drop(db);
    }
    {
        let db = enc_db(dir.path());
        let node = db.get_node(node_id).unwrap();
        // The value is an opaque sealed envelope now (never the sentinel).
        match node.properties.get("email") {
            Some(PropertyValue::Bytes(b)) => {
                assert!(
                    super::envelope::is_envelope(b),
                    "erased value stays a SUBJ envelope"
                );
                assert_ne!(
                    b.as_ref(),
                    SENTINEL.as_bytes(),
                    "erased read must never surface the plaintext sentinel"
                );
            }
            other => panic!("expected opaque sealed bytes after erase, got {other:?}"),
        }
        // The side-channel reports the erasure explicitly (never fabricated).
        let (_props, status) = db.materialize_shred(node_id.as_u64(), node.properties.clone());
        assert!(
            matches!(
                status.get("email"),
                Some(super::ShredStatus::Erased { subject_id }) if subject_id == "subject-A"
            ),
            "materialize_shred must report ShredStatus::Erased for the erased key"
        );
        drop(db);
    }

    // The sentinel plaintext (and raw embedding floats) must be absent from EVERY
    // in-scope persisted artifact.
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
    assert_no_designated_plaintext(dir.path(), &embedding_bytes(&embedding));
}

// ── Sentinel byte-scan: silent-bypass guard even BEFORE erase ──────

#[test]
fn sealed_plaintext_never_hits_any_tier_pre_or_post_erase() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let db = enc_db(dir.path());
        node_id = db
            .create_node("Doc", PropertyMapBuilder::new().insert("k", "v").build())
            .unwrap();
        db.designate_subject("S", vec![DesignationTarget::WholeNode(node_id.as_u64())])
            .unwrap();
        db.replace_node(
            node_id,
            "Doc",
            PropertyMapBuilder::new().insert("secret", SENTINEL).build(),
        )
        .unwrap();
        drop(db);
    }
    // Even while the subject is still ACTIVE, no tier holds the plaintext — the
    // seal happened at write, before any tier saw the value.
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());

    {
        let db = enc_db(dir.path());
        db.erase_subject("S").unwrap();
        drop(db);
    }
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
}

// ── Blast radius: erase A leaves B byte-identical ──────────────────

#[test]
fn blast_radius_erase_a_leaves_b_intact() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel_b = "SUBJECT_B_PLAINTEXT_stays_readable_7f1e";
    let (id_a, id_b);
    {
        let db = enc_db(dir.path());
        id_a = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        id_b = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        db.designate_subject("A", vec![DesignationTarget::WholeNode(id_a.as_u64())])
            .unwrap();
        db.designate_subject("B", vec![DesignationTarget::WholeNode(id_b.as_u64())])
            .unwrap();
        db.replace_node(
            id_a,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        db.replace_node(
            id_b,
            "Person",
            PropertyMapBuilder::new()
                .insert("email", sentinel_b)
                .build(),
        )
        .unwrap();
        db.erase_subject("A").unwrap();
        drop(db);
    }
    let db = enc_db(dir.path());
    // B is untouched: reads back byte-identical plaintext.
    let node_b = db.get_node(id_b).unwrap();
    assert_eq!(
        node_b.properties.get("email"),
        Some(&PropertyValue::from(sentinel_b.to_string())),
        "erasing A must not affect B's plaintext"
    );
    // A reads the erased marker (never the sentinel).
    let node_a = db.get_node(id_a).unwrap();
    let (_p, status) = db.materialize_shred(id_a.as_u64(), node_a.properties);
    assert!(matches!(
        status.get("email"),
        Some(super::ShredStatus::Erased { .. })
    ));
    // A's sentinel absent everywhere; B's plaintext DOES persist (it was not
    // sealed under A, and B is still active so its stored bytes are B-sealed —
    // NOT the raw sentinel_b; assert only A's leak-freedom here).
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
}

// ── __aletheia_ns coexistence: namespace marker stays cleartext ────

#[test]
fn namespace_marker_coexists_with_sealing() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let db = enc_db(dir.path());
        node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("k", "v").build())
            .unwrap();
        db.designate_subject("NS", vec![DesignationTarget::WholeNode(node_id.as_u64())])
            .unwrap();
        // Write via the namespace-stamping path so `__aletheia_ns` rides along.
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        drop(db);
    }
    let db = enc_db(dir.path());
    let node = db.get_node(node_id).unwrap();
    // The designated user property is sealed-then-unsealed to plaintext.
    assert_eq!(
        node.properties.get("email"),
        Some(&PropertyValue::from(SENTINEL.to_string()))
    );
    // Any reserved key present must remain cleartext (never a SUBJ envelope).
    for (key, value) in node.properties.iter() {
        let key_str = crate::core::interning::GLOBAL_INTERNER
            .resolve_with(*key, |s| s.to_string())
            .unwrap();
        if super::designation::is_reserved_key(&key_str)
            && let PropertyValue::Bytes(b) = value
        {
            assert!(
                !super::envelope::is_envelope(b),
                "reserved key {key_str} must never be sealed"
            );
        }
    }
    drop(db);
}

// ── HNSW exclusion: designated embedding is not searchable ─────────

#[test]
fn designated_embedding_excluded_from_hnsw() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
        .unwrap();
    let embedding = sentinel_embedding();

    // Non-designated node A with the embedding -> indexed.
    let id_a = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "A")
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    // Designated node D: placeholder -> designate -> replace with embedding, so
    // the sealed vector never enters the plaintext HNSW.
    let id_d = db
        .create_node("Doc", PropertyMapBuilder::new().insert("name", "D").build())
        .unwrap();
    db.designate_subject("D", vec![DesignationTarget::WholeNode(id_d.as_u64())])
        .unwrap();
    db.replace_node(
        id_d,
        "Doc",
        PropertyMapBuilder::new()
            .insert("name", "D")
            .insert_vector("embedding", &embedding)
            .build(),
    )
    .unwrap();

    // Query node Q (non-designated) with the same embedding.
    let id_q = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("name", "Q")
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    let results = db
        .similarity_search(crate::db::SimilarityQuery::from_node(id_q).k(10))
        .unwrap();
    let ids: Vec<_> = results.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&id_a), "non-designated A must be searchable");
    assert!(
        !ids.contains(&id_d),
        "designated D's embedding must be excluded from the plaintext HNSW"
    );
}

// ── MCP erased-marker shape ────────────────────────────────────────

#[cfg(feature = "mcp-server")]
#[test]
fn mcp_renders_erased_marker_for_erased_designated_property() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let db = enc_db(dir.path());
        node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("k", "v").build())
            .unwrap();
        db.designate_subject("E", vec![DesignationTarget::WholeNode(node_id.as_u64())])
            .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        db.erase_subject("E").unwrap();
        drop(db);
    }
    let db = Arc::new(enc_db(dir.path()));
    let server = crate::mcp::AletheiaMcpServer::new(Arc::clone(&db));
    let node = db.get_node(node_id).unwrap();
    let json = server.property_map_to_json_for_test(&node.properties, false);
    let email = json.get("email").expect("email key present");
    assert_eq!(email.get("type").and_then(|v| v.as_str()), Some("sealed"));
    assert_eq!(email.get("erased").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(email.get("subject_id").and_then(|v| v.as_str()), Some("E"));
    // Structure survives: it is still a JSON object, never the plaintext.
    assert!(!format!("{json:?}").contains(SENTINEL));
}

// ── No deadlock: designated write concurrent with read ─────────────

#[test]
fn concurrent_designated_write_and_read_no_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(enc_db(dir.path()));
    let seed = db
        .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
        .unwrap();
    db.designate_subject("C", vec![DesignationTarget::WholeNode(seed.as_u64())])
        .unwrap();
    db.replace_node(
        seed,
        "Person",
        PropertyMapBuilder::new().insert("email", SENTINEL).build(),
    )
    .unwrap();

    let writer_db = Arc::clone(&db);
    let writer = std::thread::spawn(move || {
        for i in 0..50 {
            let id = writer_db
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("i", i as i64).build(),
                )
                .unwrap();
            writer_db
                .designate_subject(
                    format!("C{i}"),
                    vec![DesignationTarget::WholeNode(id.as_u64())],
                )
                .unwrap();
            writer_db
                .replace_node(
                    id,
                    "Person",
                    PropertyMapBuilder::new().insert("email", SENTINEL).build(),
                )
                .unwrap();
        }
    });
    let reader_db = Arc::clone(&db);
    let reader = std::thread::spawn(move || {
        for _ in 0..200 {
            let node = reader_db.get_node(seed).unwrap();
            assert_eq!(
                node.properties.get("email"),
                Some(&PropertyValue::from(SENTINEL.to_string()))
            );
        }
    });
    writer.join().unwrap();
    reader.join().unwrap();
}

// ── Non-designated zero-regression ─────────────────────────────────

#[test]
fn non_designated_write_read_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "plain-alice")
                .insert("age", 30_i64)
                .build(),
        )
        .unwrap();
    let node = db.get_node(id).unwrap();
    // Values are exactly as written — no envelope, no transformation.
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::from("plain-alice".to_string()))
    );
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));
    // The side-channel is empty (nothing sealed/erased).
    let (_p, status) = db.materialize_shred(id.as_u64(), node.properties.clone());
    assert!(
        status.is_empty(),
        "non-designated read yields no shred status"
    );
    // No value in a non-designated node is a SUBJ envelope.
    for value in node.properties.values() {
        if let PropertyValue::Bytes(b) = value {
            assert!(
                !super::envelope::is_envelope(b),
                "a non-designated write must never produce a sealed envelope"
            );
        }
    }
}
