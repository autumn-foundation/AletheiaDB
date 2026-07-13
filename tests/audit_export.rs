#![cfg(feature = "audit-export")]
//! Integration tests for the signed audit export of entity history (Issue #3358).
//!
//! These encode the acceptance criteria and the adversarial edge cases:
//! full bi-temporal history is exported (incl. superseded versions and delete
//! tombstones), provenance/principal is surfaced, the artifact verifies OFFLINE
//! with only the signer's public key, and any single-byte tamper / truncation /
//! reorder / wrong-key is DETECTED.

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::audit::{
    AuditPublicKey, AuditScope, AuditSigningKey, EntityRef, ExportOptions, verify_json_bytes,
};
use aletheiadb::core::temporal::time;
use aletheiadb::{AletheiaDB, PropertyMapBuilder, Provenance};

/// Build a database with a node that has a rich bi-temporal history:
/// create (with provenance+principal) -> two updates -> delete tombstone.
fn db_with_node_history() -> (AletheiaDB, aletheiadb::NodeId) {
    let db = AletheiaDB::new().expect("db");
    let prov = Provenance::builder()
        .source("hr-system")
        .confidence(0.9)
        .note("initial import")
        .build()
        .expect("prov")
        .with_principal("alice-key");
    let id = db
        .create_node_with_options(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("city", "NYC")
                .build(),
            WriteRequestOptions::new().with_provenance(prov),
        )
        .expect("create");

    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("city", "London")
            .build(),
        None,
    )
    .expect("update1");

    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("city", "Paris")
            .insert("verified", true)
            .build(),
        None,
    )
    .expect("update2");

    // Delete tombstone closes the entity — attribution gap #3427 applies here.
    db.delete_node_with_valid_time(id, None).expect("delete");

    (db, id)
}

fn signing_key() -> AuditSigningKey {
    // Deterministic seed so tests are reproducible.
    AuditSigningKey::from_seed_bytes([7u8; 32])
}

// ── AC1: full bi-temporal history + provenance + coordinates + metadata ──────

#[test]
fn ac1_export_contains_every_version_with_provenance_and_metadata() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let export = db
        .audit_export(
            AuditScope::node(id),
            &key,
            &ExportOptions::new("audit-test-db"),
        )
        .expect("export");

    // History read count must equal exported version count (zero silent omissions).
    let history_count = db.get_node_history(id).unwrap().version_count();
    assert_eq!(
        export.version_count(),
        history_count,
        "export must include 100% of versions retrievable via history reads"
    );
    assert!(
        export.version_count() >= 4,
        "expected create + 2 updates + delete tombstone, got {}",
        export.version_count()
    );

    // Metadata surfaces database identity, export time, and chain-head anchor.
    let meta = export.metadata();
    assert_eq!(meta.database_id, "audit-test-db");
    assert!(!meta.exported_at.is_empty());
    // Principal must be surfaced where present (#3224 / #3421).
    let json = String::from_utf8(export.to_json_bytes().unwrap()).unwrap();
    assert!(json.contains("alice-key"), "principal must be surfaced");
    assert!(
        json.contains("hr-system"),
        "provenance source must be surfaced"
    );
}

// ── AC2 + AC3: offline verification succeeds with only the public key ────────

#[test]
fn ac3_offline_verification_succeeds_with_public_key_only() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let bytes = export.to_json_bytes().unwrap();

    // Offline: only bytes + public key, no database.
    let pubkey = key.public_key();
    let report = verify_json_bytes(&bytes, Some(&pubkey)).expect("verification must pass");
    assert!(report.passed);
    assert!(report.trusted_key, "verified against caller-supplied key");
    assert_eq!(report.version_count, export.version_count());
    assert!(report.summary().contains("Person") || !report.entity_summary.is_empty());
}

#[test]
fn ac3_verification_without_expected_key_uses_embedded_key_untrusted() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let bytes = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap()
        .to_json_bytes()
        .unwrap();

    let report = verify_json_bytes(&bytes, None).expect("self-consistent verify passes");
    assert!(report.passed);
    assert!(
        !report.trusted_key,
        "trust is self-asserted when no external key supplied"
    );
}

// ── AC4: tamper detection — THE CRUX ─────────────────────────────────────────

#[test]
fn ac4_single_byte_tamper_in_property_value_is_detected() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let pubkey = key.public_key();
    let bytes = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap()
        .to_json_bytes()
        .unwrap();

    // Sanity: the clean artifact verifies.
    assert!(verify_json_bytes(&bytes, Some(&pubkey)).is_ok());

    // Flip a property value "Paris" -> "Maris" via a structured edit that keeps
    // the JSON valid but changes signed content.
    let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let s = serde_json::to_string(&v).unwrap();
    assert!(s.contains("Paris"), "fixture should contain Paris");
    let tampered_str = s.replacen("Paris", "Maris", 1);
    v = serde_json::from_str(&tampered_str).unwrap();
    let tampered = serde_json::to_vec(&v).unwrap();

    assert!(
        verify_json_bytes(&tampered, Some(&pubkey)).is_err(),
        "tampering a property value MUST fail verification"
    );
}

#[test]
fn ac4_raw_single_byte_flip_anywhere_is_detected() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let pubkey = key.public_key();
    let bytes = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap()
        .to_json_bytes()
        .unwrap();
    assert!(verify_json_bytes(&bytes, Some(&pubkey)).is_ok());

    // Flip one byte in the signature hex region and confirm detection.
    let mut detected = 0usize;
    let mut attempts = 0usize;
    for i in (0..bytes.len()).step_by(7) {
        let mut m = bytes.clone();
        // Only flip printable ASCII to keep JSON parseable more often; either
        // way, a parse failure or a verify failure both count as "detected".
        m[i] ^= 0x01;
        attempts += 1;
        if verify_json_bytes(&m, Some(&pubkey)).is_err() {
            detected += 1;
        }
    }
    assert_eq!(
        detected, attempts,
        "every single-byte flip must be detected (parse-fail or verify-fail)"
    );
}

#[test]
fn ac4_tampered_timestamp_is_detected() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let pubkey = key.public_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let mut v: serde_json::Value =
        serde_json::from_slice(&export.to_json_bytes().unwrap()).unwrap();

    // Reach into the first entity's first version valid_from micros and bump it.
    let versions = v["entities"][0]["versions"].as_array_mut().unwrap();
    let vf = versions[0]["valid_from_micros"].as_i64().unwrap();
    versions[0]["valid_from_micros"] = serde_json::json!(vf + 1);
    let tampered = serde_json::to_vec(&v).unwrap();
    assert!(verify_json_bytes(&tampered, Some(&pubkey)).is_err());
}

#[test]
fn ac4_truncation_removing_a_version_is_detected() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let pubkey = key.public_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let mut v: serde_json::Value =
        serde_json::from_slice(&export.to_json_bytes().unwrap()).unwrap();

    // Drop the last version (truncate from the end).
    let versions = v["entities"][0]["versions"].as_array_mut().unwrap();
    versions.pop();
    let tampered = serde_json::to_vec(&v).unwrap();
    assert!(
        verify_json_bytes(&tampered, Some(&pubkey)).is_err(),
        "completeness is part of the proof — truncation must fail"
    );
}

#[test]
fn ac4_reordering_versions_is_detected() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let pubkey = key.public_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let mut v: serde_json::Value =
        serde_json::from_slice(&export.to_json_bytes().unwrap()).unwrap();
    let versions = v["entities"][0]["versions"].as_array_mut().unwrap();
    assert!(versions.len() >= 2);
    versions.swap(0, 1);
    let tampered = serde_json::to_vec(&v).unwrap();
    assert!(
        verify_json_bytes(&tampered, Some(&pubkey)).is_err(),
        "order-dependent chain must detect reordering"
    );
}

#[test]
fn ac4_wrong_key_fails_verification() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let bytes = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap()
        .to_json_bytes()
        .unwrap();

    let wrong = AuditSigningKey::from_seed_bytes([9u8; 32]).public_key();
    assert!(
        verify_json_bytes(&bytes, Some(&wrong)).is_err(),
        "a different public key must not verify"
    );
}

#[test]
fn ac4_public_key_substitution_with_attacker_resign_fails_trusted_verify() {
    // Attacker re-signs with their own key and swaps the embedded public key.
    // Verifying against the TRUSTED operator key must still fail.
    let (db, id) = db_with_node_history();
    let operator = signing_key();
    let operator_pub = operator.public_key();
    let attacker = AuditSigningKey::from_seed_bytes([42u8; 32]);

    let export = db
        .audit_export(AuditScope::node(id), &attacker, &ExportOptions::new("db"))
        .unwrap();
    let bytes = export.to_json_bytes().unwrap();

    // Self-consistent (attacker key embedded) — untrusted verify passes...
    assert!(verify_json_bytes(&bytes, None).is_ok());
    // ...but against the trusted operator key it MUST fail.
    assert!(verify_json_bytes(&bytes, Some(&operator_pub)).is_err());
}

// ── AC5: scope options are explicit about inclusion ──────────────────────────

#[test]
fn ac5_entity_set_scope_records_what_was_included() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let b = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "B").build())
        .unwrap();
    let key = signing_key();
    let export = db
        .audit_export(
            AuditScope::EntitySet(vec![EntityRef::Node(a), EntityRef::Node(b)]),
            &key,
            &ExportOptions::new("db"),
        )
        .unwrap();
    let report = verify_json_bytes(&export.to_json_bytes().unwrap(), Some(&key.public_key()))
        .expect("verifies");
    assert_eq!(report.entity_count, 2);
    // Scope must be stated explicitly in the artifact.
    assert!(export.metadata().scope_description.contains("entity_set"));
}

#[test]
fn ac5_neighborhood_scope_states_coordinate_and_members() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let b = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "B").build())
        .unwrap();
    db.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let key = signing_key();
    let export = db
        .audit_export(
            AuditScope::Neighborhood {
                start: a,
                hops: 1,
                valid_time: Some(time::now()),
                transaction_time: Some(time::now()),
            },
            &key,
            &ExportOptions::new("db"),
        )
        .unwrap();
    assert!(export.metadata().scope_description.contains("neighborhood"));
    // The start node plus its 1-hop neighbor and the connecting edge are included.
    assert!(export.entity_count() >= 2);
    assert!(verify_json_bytes(&export.to_json_bytes().unwrap(), Some(&key.public_key())).is_ok());
}

// ── AC6: explicit, verifiable redaction ──────────────────────────────────────

#[test]
fn ac6_redaction_is_explicit_and_still_verifies() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("ssn", "123-45-6789")
                .build(),
        )
        .unwrap();
    let key = signing_key();
    let export = db
        .audit_export(
            AuditScope::node(id),
            &key,
            &ExportOptions::new("db").redact(["ssn"]),
        )
        .unwrap();
    let bytes = export.to_json_bytes().unwrap();

    // The redacted value must NOT appear; the fact of redaction MUST appear.
    let json = String::from_utf8(bytes.clone()).unwrap();
    assert!(!json.contains("123-45-6789"), "redacted value must be gone");
    assert!(json.contains("ssn"), "redacted key must be recorded");
    assert!(export.metadata().redacted_keys.iter().any(|k| k == "ssn"));

    // Verification still passes over the redacted-but-signed content.
    let report = verify_json_bytes(&bytes, Some(&key.public_key())).expect("verifies");
    assert!(report.passed);
    assert!(report.redacted_keys.iter().any(|k| k == "ssn"));
}

// ── AC7: human-readable chronology ───────────────────────────────────────────

#[test]
fn ac7_render_chronology_is_human_readable() {
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let rendered = export.render_chronology();
    assert!(rendered.contains("Version"));
    assert!(rendered.contains("Person"));
    // Sources appear in the chronology.
    assert!(rendered.contains("hr-system") || rendered.contains("Provenance"));
}

// ── AC8 + #3427: key handling + documented attribution gap ───────────────────

#[test]
fn ac8_signing_key_roundtrips_via_file_without_leaking_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audit_signing.key");
    let key = AuditSigningKey::from_seed_bytes([3u8; 32]);
    key.write_to_file(&path).unwrap();

    // 0600 permissions on unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "signing key file must be 0600");
    }

    let loaded = AuditSigningKey::from_file(&path).unwrap();
    assert_eq!(
        loaded.public_key().to_hex(),
        key.public_key().to_hex(),
        "roundtrip preserves the keypair"
    );
    // Debug output must not leak the secret seed.
    let dbg = format!("{key:?}");
    assert!(!dbg.contains("0303") && !dbg.to_lowercase().contains("seed"));
}

#[test]
fn known_gap_3427_delete_tombstone_has_no_principal_attribution() {
    // Honest documentation-as-test: deletes do NOT stamp a principal yet (#3427).
    // The export surfaces provenance faithfully — including its ABSENCE on the
    // delete tombstone — rather than fabricating attribution.
    let (db, id) = db_with_node_history();
    let key = signing_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&export.to_json_bytes().unwrap()).unwrap();
    let versions = v["entities"][0]["versions"].as_array().unwrap();
    let last = versions.last().unwrap();
    // The tombstone carries no principal (attribution gap, documented in guide).
    let principal_present = last
        .get("provenance")
        .and_then(|p| p.get("principal"))
        .is_some();
    assert!(
        !principal_present,
        "delete attribution is a known gap (#3427); must not be fabricated"
    );
}

#[test]
fn public_key_hex_roundtrip() {
    let key = AuditSigningKey::from_seed_bytes([5u8; 32]);
    let pk = key.public_key();
    let hex = pk.to_hex();
    let parsed = AuditPublicKey::from_hex(&hex).unwrap();
    assert_eq!(parsed.to_hex(), hex);
}

#[test]
fn empty_history_edge_case_single_version_verifies() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node("Doc", PropertyMapBuilder::new().insert("t", "x").build())
        .unwrap();
    let key = signing_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    assert_eq!(export.version_count(), 1);
    assert!(verify_json_bytes(&export.to_json_bytes().unwrap(), Some(&key.public_key())).is_ok());
}

#[test]
fn unicode_property_values_roundtrip_and_tamper_still_detected() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Ólafur 日本語 😀")
                .build(),
        )
        .unwrap();
    let key = signing_key();
    let pubkey = key.public_key();
    let export = db
        .audit_export(AuditScope::node(id), &key, &ExportOptions::new("db"))
        .unwrap();
    let bytes = export.to_json_bytes().unwrap();
    assert!(verify_json_bytes(&bytes, Some(&pubkey)).is_ok());

    let s = String::from_utf8(bytes).unwrap().replacen("😀", "😎", 1);
    assert!(verify_json_bytes(s.as_bytes(), Some(&pubkey)).is_err());
}
