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

// ── helper: raw stored value bypassing the unseal hook ─────────────

/// Read the RAW stored value of `key` on a node straight from the current tier,
/// BYPASSING the crypto-shred unseal hook in [`AletheiaDB::get_node`]. On a cold
/// reopen this is the **at-rest-decrypted but still crypto-shred-sealed** value,
/// so asserting it is a `SUBJ` envelope proves sealing happened *independently*
/// of at-rest encryption (the RAM current tier is not at-rest-encrypted; a cold
/// reopen has already reversed the at-rest layer on load).
fn raw_stored_node_value(
    db: &AletheiaDB,
    node_id: crate::core::NodeId,
    key: &str,
) -> Option<PropertyValue> {
    let node = db.current.get_node(node_id).unwrap();
    node.properties.get(key).cloned()
}

fn raw_stored_edge_value(
    db: &AletheiaDB,
    edge_id: crate::core::EdgeId,
    key: &str,
) -> Option<PropertyValue> {
    let edge = db.current.get_edge(edge_id).unwrap();
    edge.properties.get(key).cloned()
}

// ── M1: FAIL-CLOSED erase-vs-seal race ─────────────────────────────

/// A write that reaches the seal hook carrying a property designated under a
/// subject that was erased after the write was buffered must **abort** the
/// commit (fail-closed) — sealing is impossible (DEK destroyed) and persisting
/// plaintext of an erased subject is forbidden. Drives the exact `seal_map` code
/// path deterministically: buffer a designated op, erase the subject mid-tx, then
/// let the commit run the seal step.
#[test]
fn write_after_erasure_aborts_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(enc_db(dir.path()));
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("stage", "placeholder")
                .build(),
        )
        .unwrap();
    db.designate_subject("S", vec![DesignationTarget::WholeNode(node_id.as_u64())])
        .unwrap();

    // Buffer a write to the designated node, then erase S BEFORE the buffered
    // write commits. The commit's seal step now sees S erased and must abort.
    let db2 = Arc::clone(&db);
    let result: Result<(), crate::core::error::Error> = db.write(|tx| {
        use crate::api::WriteOps;
        tx.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )?;
        // Erase mid-transaction: the shared CryptoShredState flips S -> Erased,
        // and its DEK is destroyed. (erase takes only the keyring lock + file
        // I/O — no write-path lock — so there is no deadlock with the in-flight
        // transaction.)
        db2.erase_subject("S").expect("erase should succeed");
        Ok(())
    });

    assert!(
        result.is_err(),
        "a write carrying a property covered only by an erased subject must ABORT (fail-closed)"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, crate::core::error::Error::FailedPrecondition(_)),
        "fail-closed abort must be a FailedPrecondition (non-retriable), got {err:?}"
    );

    // No plaintext landed: current state is still the placeholder (the write was
    // rolled back), and the designated value was never persisted as plaintext.
    let raw = raw_stored_node_value(&db, node_id, "email");
    assert_ne!(
        raw,
        Some(PropertyValue::from(SENTINEL.to_string())),
        "aborted write must not persist the designated plaintext into current state"
    );
    // And the sentinel is absent from EVERY persisted artifact (nothing was
    // sealed nor WAL-logged for the aborted tx).
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
}

// ── M2: sentinel scan ISOLATES crypto-shred from at-rest encryption ─

/// The recursive on-disk sentinel scan alone is not load-bearing under this
/// suite's `enc_config`: at-rest encryption already turns every persisted byte
/// into component-DEK ciphertext, so the raw scan would pass even if `seal_map`
/// were a no-op. This test makes the guarantee load-bearing:
///
/// (a) POSITIVE: the designated property is stored as a `SUBJ` envelope, read
///     through the storage path that reverses the at-rest layer but NOT the
///     crypto-shred seal (`db.current.get_node`, bypassing the unseal hook) —
///     proving **sealing**, not at-rest encryption, produced the ciphertext; and
/// (b) the inner (at-rest-decrypted) envelope bytes do NOT contain the plaintext
///     sentinel, pre- and post-erase — proving the seal payload is ciphertext.
///
/// The raw recursive scan is kept too, as a belt-and-suspenders check.
#[test]
fn sealing_isolated_from_at_rest_encryption() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let db = enc_db(dir.path());
        node_id = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new().insert("stage", "ph").build(),
            )
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

    // Cold reopen: index persistence has reversed the AT-REST layer on load, so
    // `db.current.get_node` yields the stored-but-at-rest-decrypted value.
    {
        let db = enc_db(dir.path());
        // (a) POSITIVE: the stored value is a SUBJ envelope (sealing happened).
        let stored =
            raw_stored_node_value(&db, node_id, "secret").expect("secret property must be present");
        let PropertyValue::Bytes(ref env) = stored else {
            panic!("designated property must be stored as sealed Bytes, got {stored:?}");
        };
        assert!(
            super::envelope::is_envelope(env),
            "designated property must be a SUBJ envelope on disk — proves crypto-shred SEALING, \
             not merely at-rest encryption, is responsible for the ciphertext"
        );
        // (b) The inner (at-rest-decrypted) envelope bytes are ciphertext: they
        // do NOT contain the plaintext sentinel. This makes the negative scan
        // load-bearing (it decrypts the at-rest layer, then checks the payload).
        assert!(
            !env.windows(SENTINEL.len())
                .any(|w| w == SENTINEL.as_bytes()),
            "the at-rest-decrypted SUBJ envelope must NOT contain the plaintext sentinel \
             (its payload is sealed ciphertext)"
        );
        // While active, the public read path unseals it back to plaintext.
        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("secret"),
            Some(&PropertyValue::from(SENTINEL.to_string()))
        );
        drop(db);
    }

    // Erase, cold reopen: the stored value is STILL a SUBJ envelope whose inner
    // bytes never expose the plaintext.
    {
        let db = enc_db(dir.path());
        db.erase_subject("S").unwrap();
        drop(db);
    }
    {
        let db = enc_db(dir.path());
        let stored = raw_stored_node_value(&db, node_id, "secret").unwrap();
        let PropertyValue::Bytes(ref env) = stored else {
            panic!("post-erase value must remain sealed Bytes");
        };
        assert!(super::envelope::is_envelope(env));
        assert!(
            !env.windows(SENTINEL.len())
                .any(|w| w == SENTINEL.as_bytes()),
            "post-erase envelope payload must never contain the plaintext sentinel"
        );
        drop(db);
    }

    // Belt-and-suspenders: the raw (still at-rest-encrypted) scan is clean too.
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
}

// ── M3: EDGE end-to-end (AC1 covers edges) ─────────────────────────

/// AC1 designates whole entities AND/OR property keys for **edges** too. Seal a
/// designated edge property at write, round-trip it as plaintext through a cold
/// restart while the key is present, then erase and confirm the edge property
/// reads as the erased marker (never the sentinel).
#[test]
fn edge_e2e_seal_unseal_survives_restart_then_erase() {
    let dir = tempfile::tempdir().unwrap();
    let (src, tgt, edge_id);
    {
        let db = enc_db(dir.path());
        src = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
            .unwrap();
        tgt = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "b").build())
            .unwrap();
        // Placeholder edge -> designate -> replace so the sealed version carries
        // the sentinel (robust to whatever edge id is assigned).
        edge_id = db
            .create_edge(
                src,
                tgt,
                "KNOWS",
                PropertyMapBuilder::new().insert("stage", "ph").build(),
            )
            .unwrap();
        db.designate_subject("EG", vec![DesignationTarget::WholeEdge(edge_id.as_u64())])
            .unwrap();
        db.replace_edge(
            edge_id,
            PropertyMapBuilder::new()
                .insert("relnote", SENTINEL)
                .build(),
        )
        .unwrap();
        // Sealed on disk (bypass the unseal hook to observe the raw envelope).
        let stored = raw_stored_edge_value(&db, edge_id, "relnote").unwrap();
        assert!(
            matches!(&stored, PropertyValue::Bytes(b) if super::envelope::is_envelope(b)),
            "designated edge property must be sealed at write, got {stored:?}"
        );
        drop(db);
    }

    // Cold reopen, key present: the edge property reads back as plaintext.
    {
        let db = enc_db(dir.path());
        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.properties.get("relnote"),
            Some(&PropertyValue::from(SENTINEL.to_string())),
            "cold reopen with key present must return edge plaintext"
        );
        drop(db);
    }

    // Erase, cold reopen: the edge property reads as the erased marker, never
    // the sentinel — surfaced via the public edge_shred_status accessor.
    {
        let db = enc_db(dir.path());
        db.erase_subject("EG").unwrap();
        drop(db);
    }
    {
        let db = enc_db(dir.path());
        let edge = db.get_edge(edge_id).unwrap();
        match edge.properties.get("relnote") {
            Some(PropertyValue::Bytes(b)) => {
                assert!(
                    super::envelope::is_envelope(b),
                    "erased edge value stays a SUBJ envelope"
                );
                assert_ne!(b.as_ref(), SENTINEL.as_bytes());
            }
            other => panic!("expected opaque sealed edge bytes after erase, got {other:?}"),
        }
        let status = db.edge_shred_status(edge_id).unwrap();
        assert!(
            matches!(
                status.get("relnote"),
                Some(super::ShredStatus::Erased { subject_id }) if subject_id == "EG"
            ),
            "public edge_shred_status must report Erased for the erased edge property"
        );
        drop(db);
    }
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
}

// ── M4: FIELD-LEVEL / MIXED designation (AC1 specific property keys) ─

/// Designating only `email` under a subject must seal exactly that key: a
/// sibling `name` stays cleartext, reads unaffected, and only `email` is erased
/// after erasure — proving property-key-scoped designation end-to-end.
#[test]
fn field_level_designation_seals_only_named_key() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let db = enc_db(dir.path());
        node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("stage", "ph").build(),
            )
            .unwrap();
        db.designate_subject(
            "F",
            vec![DesignationTarget::NodeProperties(
                node_id.as_u64(),
                vec!["email".to_string()],
            )],
        )
        .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new()
                .insert("email", SENTINEL)
                .insert("name", "plain-name")
                .build(),
        )
        .unwrap();

        // On disk: `email` is a SUBJ envelope; `name` is cleartext.
        let stored_email = raw_stored_node_value(&db, node_id, "email").unwrap();
        assert!(
            matches!(&stored_email, PropertyValue::Bytes(b) if super::envelope::is_envelope(b)),
            "designated `email` must be sealed, got {stored_email:?}"
        );
        assert_eq!(
            raw_stored_node_value(&db, node_id, "name"),
            Some(PropertyValue::from("plain-name".to_string())),
            "undesignated sibling `name` must stay cleartext on disk"
        );

        // Read back: `email` unseals to the sentinel, `name` is plaintext.
        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("email"),
            Some(&PropertyValue::from(SENTINEL.to_string()))
        );
        assert_eq!(
            node.properties.get("name"),
            Some(&PropertyValue::from("plain-name".to_string()))
        );
        // Active subject: shred status is empty (nothing erased yet).
        assert!(db.node_shred_status(node_id).unwrap().is_empty());
        drop(db);
    }

    // After erase: only `email` is erased-marked; `name` stays plaintext.
    {
        let db = enc_db(dir.path());
        db.erase_subject("F").unwrap();
        drop(db);
    }
    {
        let db = enc_db(dir.path());
        let status = db.node_shred_status(node_id).unwrap();
        assert_eq!(status.len(), 1, "only the designated `email` key is erased");
        assert!(matches!(
            status.get("email"),
            Some(super::ShredStatus::Erased { subject_id }) if subject_id == "F"
        ));
        assert!(!status.contains_key("name"), "`name` was never designated");
        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.properties.get("name"),
            Some(&PropertyValue::from("plain-name".to_string())),
            "undesignated `name` stays plaintext after erasing `email`'s subject"
        );
        drop(db);
    }
}

// ── M5: PUBLIC Rust erased-marker accessor ─────────────────────────

/// The typed erased marker the design promised must be reachable through a
/// PUBLIC Rust method — `AletheiaDB::node_shred_status` — not only the
/// pub(crate) `materialize_shred`.
#[test]
fn public_node_shred_status_reports_erased_through_public_api() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("stage", "ph").build(),
        )
        .unwrap();
    db.designate_subject("P", vec![DesignationTarget::WholeNode(node_id.as_u64())])
        .unwrap();
    db.replace_node(
        node_id,
        "Person",
        PropertyMapBuilder::new().insert("email", SENTINEL).build(),
    )
    .unwrap();

    // Active: public accessor reports nothing erased.
    assert!(
        db.node_shred_status(node_id).unwrap().is_empty(),
        "active subject: public accessor reports no erased keys"
    );

    db.erase_subject("P").unwrap();

    // Erased: public accessor surfaces the typed ShredStatus::Erased.
    let status = db.node_shred_status(node_id).unwrap();
    assert!(
        matches!(
            status.get("email"),
            Some(super::ShredStatus::Erased { subject_id }) if subject_id == "P"
        ),
        "public node_shred_status must surface ShredStatus::Erased{{subject_id}}"
    );
}

// ── m1: AS-OF read returns the erased marker (AC3 point-in-time) ────

/// A point-in-time read (`get_node_at_time`) of an erased subject must surface
/// the erased marker (opaque envelope), never the plaintext.
#[test]
fn as_of_read_returns_erased_marker() {
    use crate::core::temporal::time;

    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("stage", "ph").build(),
        )
        .unwrap();
    db.designate_subject("T", vec![DesignationTarget::WholeNode(node_id.as_u64())])
        .unwrap();
    db.replace_node(
        node_id,
        "Person",
        PropertyMapBuilder::new().insert("email", SENTINEL).build(),
    )
    .unwrap();
    db.erase_subject("T").unwrap();

    let now = time::now();
    let node = db.get_node_at_time(node_id, now, now).unwrap();
    match node.properties.get("email") {
        Some(PropertyValue::Bytes(b)) => {
            assert!(
                super::envelope::is_envelope(b),
                "AS OF read of an erased subject must return the sealed envelope (erased marker)"
            );
            assert_ne!(
                b.as_ref(),
                SENTINEL.as_bytes(),
                "AS OF read must never surface the plaintext of an erased subject"
            );
        }
        other => panic!("expected opaque sealed bytes on AS OF read after erase, got {other:?}"),
    }
}

// ── m2a: update of a designated property re-seals ──────────────────

/// A PATCH update touching an already-designated property must re-seal the new
/// value (the UpdateNode path runs through `seal_buffer` too).
#[test]
fn update_of_designated_property_reseals() {
    const SENTINEL_V2: &str = "SENTINEL_GDPR_UPDATED_1b2c3d4e_PLAINTEXT_MUST_NOT_LEAK";

    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    let node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("stage", "ph").build(),
        )
        .unwrap();
    db.designate_subject("U", vec![DesignationTarget::WholeNode(node_id.as_u64())])
        .unwrap();
    // Create with the designated property sealed.
    db.replace_node(
        node_id,
        "Person",
        PropertyMapBuilder::new().insert("email", SENTINEL).build(),
    )
    .unwrap();

    // PATCH update touching the designated property with a new value.
    db.update_node_with_valid_time(
        node_id,
        PropertyMapBuilder::new()
            .insert("email", SENTINEL_V2)
            .build(),
        None,
    )
    .unwrap();

    // Still sealed on disk (the update re-sealed the new value).
    let stored = raw_stored_node_value(&db, node_id, "email").unwrap();
    assert!(
        matches!(&stored, PropertyValue::Bytes(b) if super::envelope::is_envelope(b)),
        "update of a designated property must re-seal, got {stored:?}"
    );
    // Read back: the new value unseals to plaintext.
    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.properties.get("email"),
        Some(&PropertyValue::from(SENTINEL_V2.to_string()))
    );
    // Neither the old nor the new plaintext leaks to any artifact.
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
    assert_no_designated_plaintext(dir.path(), SENTINEL_V2.as_bytes());
}

// ── m2b: foreign SUBJ-looking bytes for an UNKNOWN subject pass through ─

/// A `Bytes` value that merely *looks* like a `SUBJ` envelope but names a
/// subject the keyring does not know must be passed through untouched on read —
/// no panic, no false erased marker — even when the database has other active
/// designations (so the unseal hook actually runs).
#[test]
fn foreign_subj_bytes_unknown_subject_pass_through() {
    use crate::encryption::{Algorithm, create_cipher};
    use zeroize::Zeroizing;

    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());

    // Ensure the DB has an active designation on ANOTHER node, so the read hook
    // is engaged (has_any_designations == true) rather than short-circuiting.
    let other = db
        .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
        .unwrap();
    db.designate_subject("real", vec![DesignationTarget::WholeNode(other.as_u64())])
        .unwrap();

    // Craft a well-formed SUBJ envelope naming a subject we NEVER designate, so
    // the read hook parses the header, finds no such subject, and passes it
    // through as ordinary user bytes.
    let cipher = create_cipher(Algorithm::Aes256Gcm, &Zeroizing::new([7u8; 32]));
    let foreign = super::envelope::seal_property_value(
        &PropertyValue::from("not-ours".to_string()),
        "ghost-unknown-subject",
        1,
        b"ctx",
        cipher.as_ref(),
    )
    .unwrap();
    assert!(super::envelope::is_envelope(&foreign));

    // Write it on an UNDESIGNATED node (so seal_buffer leaves it as-is).
    let holder = db
        .create_node(
            "Blob",
            PropertyMapBuilder::new()
                .insert(
                    "payload",
                    PropertyValue::Bytes(std::sync::Arc::from(foreign.clone())),
                )
                .build(),
        )
        .unwrap();

    // Read back: the foreign bytes are returned untouched (no panic, no unseal).
    let node = db.get_node(holder).unwrap();
    assert_eq!(
        node.properties.get("payload"),
        Some(&PropertyValue::Bytes(std::sync::Arc::from(foreign.clone()))),
        "foreign SUBJ-looking bytes for an unknown subject must pass through untouched"
    );
    // And no false erased marker is fabricated for it.
    let status = db.node_shred_status(holder).unwrap();
    assert!(
        !status.contains_key("payload"),
        "unknown-subject bytes must not be reported as erased"
    );
}

// ── Issue #3665: keyring + designation registry fold into `.albk` ──────
//
// These exercise the v7 archive: designations, erased-state, `erased_at`, and
// attestations travel INSIDE the backup, so an active subject's designated
// property is readable after restore while an erased subject stays erased and
// its DEK is physically absent from the archive.

/// Reopen a durable database at `data_dir` (canonical durable layout) with
/// encryption keyed by `key_file`, so restored designated properties can be
/// unsealed with the same MEK the source used.
fn reopen_encrypted(data_dir: &Path, key_file: &Path) -> AletheiaDB {
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .encryption(EncryptionConfig::file_based(key_file))
        .build();
    AletheiaDB::with_unified_config(config).unwrap()
}

/// T1: an ACTIVE subject's designated property is readable again after a
/// backup→restore, because its wrapped DEK travels in the v7 archive.
#[test]
fn active_subject_designated_property_readable_after_restore() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let key_file = src.path().join("mek.key");
    let albk = src.path().join("backup.albk");

    let node_id;
    {
        let db = enc_db(src.path());
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
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        db.backup(&albk).unwrap();
        drop(db);
    }

    // Restore materializes indexes + keyring into `dst`; its own reopen is
    // unencrypted (cannot unseal), so drop it and reopen WITH the MEK.
    let restored = AletheiaDB::restore_to_data_dir(&albk, dst.path()).unwrap();
    drop(restored);

    // The keyring sidecar landed in the data dir (parent of the WAL dir).
    assert!(
        dst.path().join("subject_keyring.dat").exists(),
        "restore must write subject_keyring.dat into the data dir"
    );

    let db = reopen_encrypted(dst.path(), &key_file);
    assert!(
        db.crypto_shred.is_active("subject-A"),
        "subject-A must restore as Active"
    );
    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.properties.get("email"),
        Some(&PropertyValue::from(SENTINEL.to_string())),
        "active subject's designated property must be readable after restore"
    );
}

/// T2: an ERASED subject stays erased after restore — its designated property
/// is not readable, and `erased_at` + attestation survive.
#[test]
fn erased_subject_stays_erased_after_restore() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let key_file = src.path().join("mek.key");
    let albk = src.path().join("backup.albk");

    let node_id;
    let erased_at;
    {
        let db = enc_db(src.path());
        node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        db.designate_subject(
            "subject-E",
            vec![DesignationTarget::WholeNode(node_id.as_u64())],
        )
        .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        db.erase_subject("subject-E").unwrap();
        erased_at = db.crypto_shred.subject_erased_at("subject-E");
        assert!(erased_at.is_some(), "source erase must record erased_at");
        // Back up AFTER erase.
        db.backup(&albk).unwrap();
        drop(db);
    }

    let restored = AletheiaDB::restore_to_data_dir(&albk, dst.path()).unwrap();
    drop(restored);

    let db = reopen_encrypted(dst.path(), &key_file);
    assert!(
        db.crypto_shred.is_erased("subject-E"),
        "subject-E must restore as Erased"
    );
    // erased_at survives the round-trip.
    assert_eq!(
        db.crypto_shred.subject_erased_at("subject-E"),
        erased_at,
        "erased_at must survive backup→restore"
    );
    // The designated property is NOT readable — it stays an opaque envelope,
    // never the plaintext sentinel.
    let node = db.get_node(node_id).unwrap();
    match node.properties.get("email") {
        Some(PropertyValue::Bytes(b)) => {
            assert!(
                super::envelope::is_envelope(b),
                "erased value stays a SUBJ envelope after restore"
            );
            assert_ne!(
                b.as_ref(),
                SENTINEL.as_bytes(),
                "erased read must never surface the plaintext sentinel"
            );
        }
        other => panic!("expected opaque sealed bytes after restore, got {other:?}"),
    }
    // Attestation survives (re-erase returns the recorded one).
    let attestation = db.erase_subject("subject-E").unwrap();
    assert_eq!(attestation.subject_id, "subject-E");
}

/// T3: the erased subject's attestation verifies after restore.
#[test]
fn attestation_survives_restore_and_verifies() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let key_file = src.path().join("mek.key");
    let albk = src.path().join("backup.albk");

    {
        let db = enc_db(src.path());
        let node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        db.designate_subject(
            "subject-V",
            vec![DesignationTarget::WholeNode(node_id.as_u64())],
        )
        .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        db.erase_subject("subject-V").unwrap();
        db.backup(&albk).unwrap();
        drop(db);
    }

    let restored = AletheiaDB::restore_to_data_dir(&albk, dst.path()).unwrap();
    drop(restored);

    let db = reopen_encrypted(dst.path(), &key_file);
    // The recorded attestation comes back via the idempotent re-erase and
    // verifies against its own embedded signer public key.
    let attestation = db.erase_subject("subject-V").unwrap();
    assert!(
        attestation.verify(),
        "restored erasure attestation must verify against its embedded public key"
    );
}

/// T4: the pre-erasure wrapped-DEK ciphertext is ABSENT from the v7 archive.
#[test]
fn erased_dek_bytes_absent_from_v7_archive() {
    let src = tempfile::tempdir().unwrap();
    let albk = src.path().join("backup.albk");

    // Capture the wrapped-DEK ciphertext BEFORE erasing (never printed).
    let wrapped_dek;
    {
        let db = enc_db(src.path());
        let node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        db.designate_subject(
            "subject-D",
            vec![DesignationTarget::WholeNode(node_id.as_u64())],
        )
        .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        wrapped_dek = db
            .crypto_shred
            .subject_wrapped_dek_for_test("subject-D")
            .expect("active subject must have a wrapped DEK before erase");
        assert!(!wrapped_dek.is_empty());
        db.erase_subject("subject-D").unwrap();
        db.backup(&albk).unwrap();
        drop(db);
    }

    // STRUCTURAL proof: decode the archive and its keyring sidecar, and assert
    // the erased subject's entry carries NO wrapped key (physically removed).
    let payload = crate::storage::backup::read_artifact(&albk).expect("read v7 archive");
    assert!(
        !payload.keyring_sidecar.is_empty(),
        "v7 archive of a crypto-shred DB must carry a keyring sidecar"
    );
    // `keyring_sidecar` is `[bitcode(SubjectKeyringSidecar) || crc32 LE]`; strip
    // the 4-byte CRC before decoding.
    let sidecar_bytes = &payload.keyring_sidecar[..payload.keyring_sidecar.len() - 4];
    let sidecar: super::keyring::SubjectKeyringSidecar =
        bitcode::decode(sidecar_bytes).expect("decode keyring sidecar");
    let erased = sidecar
        .entries
        .iter()
        .find(|e| e.subject_id == "subject-D")
        .expect("erased subject entry must be present in the sidecar");
    assert!(
        erased.wrapped_key.is_none(),
        "erased subject must carry no wrapped DEK in the archived keyring"
    );

    // BYTE proof (raw archive): the captured wrapped-DEK ciphertext must not
    // appear anywhere in the compressed on-disk artifact.
    let archive = std::fs::read(&albk).unwrap();
    assert!(
        !archive
            .windows(wrapped_dek.len())
            .any(|w| w == wrapped_dek.as_slice()),
        "erased subject's wrapped-DEK ciphertext must be absent from the raw v7 archive"
    );

    // BYTE proof (decompressed payload): scan the DECOMPRESSED payload too, so
    // the absence is not vacuously true against the compressed bytes.
    let payload_bytes =
        crate::storage::backup::decompress_artifact_payload(&albk).expect("decompress .albk");
    assert!(
        !payload_bytes
            .windows(wrapped_dek.len())
            .any(|w| w == wrapped_dek.as_slice()),
        "erased subject's wrapped-DEK ciphertext must be absent from the DECOMPRESSED payload"
    );
}

/// T5: a designate→erase→backup leaves zero designated plaintext in the `.albk`
/// artifact.
///
/// The `.albk` body is zstd-COMPRESSED, so a raw byte scan of the on-disk file
/// can pass **vacuously** (a plaintext needle would not match the compressed
/// bytes even if it were logically present). This test therefore scans the
/// DECOMPRESSED payload as the primary proof of absence, keeping the raw-file
/// scan as a secondary check.
#[test]
fn sentinel_scan_albk_zero_designated_plaintext() {
    let src = tempfile::tempdir().unwrap();
    // Put the backup in its own dir so the scan targets the artifact directly.
    let backup_dir = tempfile::tempdir().unwrap();
    let albk = backup_dir.path().join("backup.albk");
    let embedding = sentinel_embedding();

    {
        let db = enc_db(src.path());
        db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
            .unwrap();
        let node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        db.designate_subject(
            "subject-S",
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
        db.erase_subject("subject-S").unwrap();
        db.backup(&albk).unwrap();
        drop(db);
    }

    // PRIMARY: scan the DECOMPRESSED payload — this is the real proof of
    // absence (the raw-file scan alone can pass vacuously on compressed bytes).
    let payload_bytes =
        crate::storage::backup::decompress_artifact_payload(&albk).expect("decompress .albk");
    let sentinel = SENTINEL.as_bytes();
    let emb_bytes = embedding_bytes(&embedding);
    assert!(
        !payload_bytes.windows(sentinel.len()).any(|w| w == sentinel),
        "designated plaintext sentinel present in DECOMPRESSED .albk payload"
    );
    assert!(
        !payload_bytes
            .windows(emb_bytes.len())
            .any(|w| w == emb_bytes.as_slice()),
        "designated embedding LE bytes present in DECOMPRESSED .albk payload"
    );

    // SECONDARY: the raw `.albk` artifact holds no designated plaintext either.
    assert_no_designated_plaintext(backup_dir.path(), SENTINEL.as_bytes());
    // And neither does the (encrypted) source tree.
    assert_no_designated_plaintext(src.path(), SENTINEL.as_bytes());
}

/// T7: a plain (no crypto-shred) database round-trips through backup→restore
/// with an empty keyring — no `subject_keyring.dat` is written and the reopen
/// has no crypto-shred state.
#[test]
fn no_cryptoshred_db_v7_roundtrip() {
    let dst = tempfile::tempdir().unwrap();
    let backup_dir = tempfile::tempdir().unwrap();
    let albk = backup_dir.path().join("plain.albk");

    let node_id = {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("Doc", PropertyMapBuilder::new().insert("k", "v").build())
            .unwrap();
        db.backup(&albk).unwrap();
        id
    };

    let db = AletheiaDB::restore_to_data_dir(&albk, dst.path()).unwrap();
    // No keyring sidecar written for a crypto-shred-free backup.
    assert!(
        !dst.path().join("subject_keyring.dat").exists(),
        "a plain-DB restore must not write subject_keyring.dat"
    );
    // Reopen has no crypto-shred state.
    assert!(
        !db.crypto_shred.has_any_designations(),
        "restored plain DB must have no designations"
    );
    // The plain node round-trips.
    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.properties.get("k"),
        Some(&PropertyValue::from("v".to_string()))
    );
}

/// Issue #3665 hardening: restoring a **keyless** (pre-v7 style) archive that
/// still carries designated (SUBJ-sealed) property bytes succeeds, leaves the
/// property sealed-**unreadable** (opaque envelope, never the plaintext), and
/// exercises the loud-warning branch in `restore_keyring_sidecar`.
#[test]
fn keyless_restore_of_sealed_property_seals_unreadable() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let key_file = src.path().join("mek.key");
    let albk = src.path().join("v7.albk");
    let keyless_albk = src.path().join("keyless.albk");

    let node_id;
    {
        let db = enc_db(src.path());
        node_id = db
            .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
            .unwrap();
        db.designate_subject(
            "subject-KL",
            vec![DesignationTarget::WholeNode(node_id.as_u64())],
        )
        .unwrap();
        db.replace_node(
            node_id,
            "Person",
            PropertyMapBuilder::new().insert("email", SENTINEL).build(),
        )
        .unwrap();
        db.backup(&albk).unwrap();
        drop(db);
    }

    // Simulate a pre-v7 (keyless) archive: read the v7 payload, strip the
    // keyring, and re-encode. The sealed property bytes remain in the graph.
    let mut payload = crate::storage::backup::read_artifact(&albk).expect("read v7 archive");
    assert!(
        !payload.keyring_sidecar.is_empty(),
        "the v7 archive must carry a keyring to strip"
    );
    payload.keyring_sidecar = Vec::new();
    crate::storage::backup::write_artifact(&payload, &keyless_albk).expect("write keyless archive");

    // Restore succeeds (the warning branch fires; the sealed prop cannot be
    // recovered without the keyring).
    let restored = AletheiaDB::restore_to_data_dir(&keyless_albk, dst.path()).unwrap();
    drop(restored);
    // No keyring sidecar is written for a keyless archive.
    assert!(
        !dst.path().join("subject_keyring.dat").exists(),
        "a keyless restore must not write subject_keyring.dat"
    );

    // Reopen WITH the MEK: the property is still an opaque SUBJ envelope,
    // never the plaintext (sealed-unreadable).
    let db = reopen_encrypted(dst.path(), &key_file);
    match db.get_node(node_id).unwrap().properties.get("email") {
        Some(PropertyValue::Bytes(b)) => {
            assert!(
                super::envelope::is_envelope(b),
                "keyless-restored designated value must remain a SUBJ envelope"
            );
            assert_ne!(
                b.as_ref(),
                SENTINEL.as_bytes(),
                "keyless-restored designated value must never be the plaintext"
            );
        }
        other => panic!("expected an opaque sealed envelope, got {other:?}"),
    }
}

/// Issue #3665 hardening: a keyless (empty-sidecar) restore into a data dir
/// that already holds a leftover `subject_keyring.dat` removes the stale file
/// so the restored (empty-crypto-shred) state wins over the stale keyring.
#[test]
fn keyless_restore_removes_stale_keyring() {
    let dst = tempfile::tempdir().unwrap();
    let backup_dir = tempfile::tempdir().unwrap();
    let albk = backup_dir.path().join("plain.albk");

    // A plain (no crypto-shred) v7 archive.
    {
        let db = AletheiaDB::new().unwrap();
        db.create_node("Doc", PropertyMapBuilder::new().insert("k", "v").build())
            .unwrap();
        db.backup(&albk).unwrap();
    }

    // Pre-seed a stale keyring in the target data dir.
    let stale = dst
        .path()
        .join(crate::db::crypto_shred::keyring::KEYRING_FILENAME);
    std::fs::write(&stale, b"stale-keyring-content").unwrap();
    assert!(stale.exists());

    let db = AletheiaDB::restore_to_data_dir(&albk, dst.path()).unwrap();
    // The stale keyring must be gone — the restored empty state wins.
    assert!(
        !stale.exists(),
        "keyless restore must remove a pre-existing stale subject_keyring.dat"
    );
    assert!(
        !db.crypto_shred.has_any_designations(),
        "restored plain DB must have no crypto-shred state"
    );
}

/// Issue #3665 hardening: a malicious v7 archive with an over-cap keyring
/// sidecar is rejected on the restore WRITE path (symmetry with the 64 MiB
/// load cap) BEFORE any oversized `subject_keyring.dat` is written.
#[test]
fn oversized_keyring_sidecar_rejected_on_restore() {
    use crate::db::crypto_shred::keyring::MAX_KEYRING_BYTES;

    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let plain_albk = src.path().join("plain.albk");
    let evil_albk = src.path().join("evil.albk");

    // Start from a valid plain v7 archive, then bloat its keyring sidecar with
    // over-cap zero bytes (no key material — just zeros).
    {
        let db = AletheiaDB::new().unwrap();
        db.create_node("Doc", PropertyMapBuilder::new().insert("k", "v").build())
            .unwrap();
        db.backup(&plain_albk).unwrap();
    }
    let mut payload =
        crate::storage::backup::read_artifact(&plain_albk).expect("read plain archive");
    payload.keyring_sidecar = vec![0u8; (MAX_KEYRING_BYTES + 1) as usize];
    crate::storage::backup::write_artifact(&payload, &evil_albk).expect("write evil archive");

    let result = AletheiaDB::restore_to_data_dir(&evil_albk, dst.path());
    assert!(
        result.is_err(),
        "an over-cap keyring sidecar must be rejected on restore"
    );
    // The over-cap sidecar is rejected BEFORE any write, so no
    // `subject_keyring.dat` may exist in the target data dir at all.
    let written = dst
        .path()
        .join(crate::db::crypto_shred::keyring::KEYRING_FILENAME);
    assert!(
        !written.exists(),
        "no subject_keyring.dat may be written when an over-cap sidecar is rejected"
    );
}

/// R2 cold clause (Issue #3665 hardening, best-effort): a sealed designated
/// property that MIGRATES to the cold redb tier leaves zero designated
/// plaintext in `cold.redb` (seal-at-write means only the opaque SUBJ envelope
/// ever reaches any tier). Cold storage is configured/attached via public /
/// test APIs only — no cold-tier SOURCE is edited.
#[test]
fn cold_tier_redb_zero_designated_plaintext() {
    use crate::storage::migration::{MigrationPolicyBuilder, MigrationService};
    use crate::storage::redb_cold_storage::RedbColdStorage;
    use crate::storage::tiered_storage::TieredStorage;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.redb");

    let db = enc_db(dir.path());
    let node_id = db
        .create_node("Person", PropertyMapBuilder::new().insert("s", "p").build())
        .unwrap();
    db.designate_subject(
        "subject-COLD",
        vec![DesignationTarget::WholeNode(node_id.as_u64())],
    )
    .unwrap();
    // v2: the sealed SENTINEL version (will become non-head and migrate cold).
    db.replace_node(
        node_id,
        "Person",
        PropertyMapBuilder::new().insert("email", SENTINEL).build(),
    )
    .unwrap();
    // v3: a newer head, so v2 is eligible to migrate to the cold tier.
    db.replace_node(
        node_id,
        "Person",
        PropertyMapBuilder::new().insert("email", "later").build(),
    )
    .unwrap();

    // Attach a cold tier and migrate everything older than the head version.
    let cold = Arc::new(RedbColdStorage::with_default_config(&cold_path).unwrap());
    let tiered = Arc::new(TieredStorage::with_default_config(Arc::clone(&cold)));
    db.__test_historical_storage()
        .write()
        .set_tiered_storage(tiered);
    let policy = MigrationPolicyBuilder::new()
        .age_threshold(Duration::ZERO)
        .build();
    let service = MigrationService::new(Arc::clone(&cold), policy);
    let migrated = db
        .__test_historical_storage()
        .write()
        .migrate_to_cold(&service)
        .unwrap();
    assert!(
        migrated >= 1,
        "at least the sealed non-head version must migrate to cold"
    );

    // Erase the subject, then flush/close the cold store so its file is on disk.
    db.erase_subject("subject-COLD").unwrap();
    drop(service);
    drop(db);
    drop(cold);

    assert!(cold_path.exists(), "cold.redb must exist after migration");
    // The scan reaches the whole data dir, including cold.redb.
    assert_no_designated_plaintext(dir.path(), SENTINEL.as_bytes());
}
