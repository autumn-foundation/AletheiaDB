//! Unit tests for audit-export internals (Issue #3358).

use super::*;
use crate::AletheiaDB;
use crate::core::property::PropertyMapBuilder;

fn key() -> AuditSigningKey {
    AuditSigningKey::from_seed_bytes([11u8; 32])
}

#[test]
fn signing_key_hex_roundtrip() {
    let k = AuditSigningKey::from_seed_hex(&crate::core::hex::encode(&[4u8; 32])).unwrap();
    assert_eq!(k.public_key().to_hex().len(), 64);
}

#[test]
fn signing_key_rejects_short_hex() {
    assert!(AuditSigningKey::from_seed_hex("abcd").is_err());
    assert!(AuditSigningKey::from_seed_hex("nothex!!").is_err());
}

#[test]
fn public_key_rejects_bad_hex() {
    assert!(AuditPublicKey::from_hex("00").is_err());
    assert!(AuditPublicKey::from_hex("zz").is_err());
}

#[test]
fn signing_key_debug_hides_secret() {
    let k = AuditSigningKey::from_seed_bytes([0xAB; 32]);
    let dbg = format!("{k:?}");
    assert!(!dbg.contains("abab"), "debug must not leak the seed");
    assert!(dbg.contains("public_key"));
}

#[test]
fn write_to_file_refuses_existing_path() {
    // Issue #3351: create_new(true) refuses to write over a pre-existing file,
    // so an attacker-planted file is never followed/truncated.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("key.hex");
    let k = key();
    k.write_to_file(&path).unwrap();
    let err = k.write_to_file(&path).unwrap_err();
    assert!(
        matches!(err, AuditError::Io(_)),
        "second write over an existing path must be refused"
    );
}

#[cfg(unix)]
#[test]
fn write_to_file_refuses_symlink_target() {
    // A symlink planted at the target must not be followed/truncated.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.txt");
    std::fs::write(&real, b"planted-secret").unwrap();
    let link = dir.path().join("link.hex");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let err = key().write_to_file(&link).unwrap_err();
    assert!(
        matches!(err, AuditError::Io(_)),
        "writing through a symlink must be refused"
    );
    // The pre-existing real file must be untouched.
    assert_eq!(std::fs::read(&real).unwrap(), b"planted-secret");
}

#[test]
fn export_is_deterministic_for_same_content_ordering() {
    // Two exports of the same entity produce identical chains (only the export
    // timestamp/anchor differ, which live in metadata) — the per-version leaves
    // must be identical.
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let k = key();
    let e1 = db
        .audit_export(AuditScope::node(id), &k, &ExportOptions::new("db"))
        .unwrap();
    let e2 = db
        .audit_export(AuditScope::node(id), &k, &ExportOptions::new("db"))
        .unwrap();
    assert_eq!(e1.chain.leaves, e2.chain.leaves);
}

#[test]
fn nonexistent_single_entity_errors_no_history() {
    let db = AletheiaDB::new().unwrap();
    let missing = crate::core::NodeId::new(999_999).unwrap();
    let err = db
        .audit_export(AuditScope::node(missing), &key(), &ExportOptions::new("db"))
        .unwrap_err();
    assert!(matches!(err, AuditError::NoHistory(_)));
}

#[test]
fn entity_set_records_missing_as_not_included() {
    let db = AletheiaDB::new().unwrap();
    let present = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let missing = crate::core::NodeId::new(888_888).unwrap();
    let export = db
        .audit_export(
            AuditScope::EntitySet(vec![EntityRef::Node(present), EntityRef::Node(missing)]),
            &key(),
            &ExportOptions::new("db"),
        )
        .unwrap();
    assert_eq!(export.entity_count(), 1);
    assert_eq!(export.metadata().scope.not_included.len(), 1);
    assert_eq!(export.metadata().scope.not_included[0].reason, "no_history");
    // Still verifies.
    assert!(export.verify(Some(&key().public_key())).is_ok());
}

#[test]
fn wrong_format_version_is_rejected() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let mut export = db
        .audit_export(AuditScope::node(id), &key(), &ExportOptions::new("db"))
        .unwrap();
    export.format_version = 999;
    let err = export.verify(Some(&key().public_key())).unwrap_err();
    assert!(matches!(err, AuditError::UnsupportedFormat { .. }));
}

#[test]
fn edge_history_is_exportable_with_endpoints() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let b = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "B").build())
        .unwrap();
    let edge = db
        .create_edge(
            a,
            b,
            "KNOWS",
            PropertyMapBuilder::new().insert("w", 1i64).build(),
        )
        .unwrap();
    let export = db
        .audit_export(AuditScope::edge(edge), &key(), &ExportOptions::new("db"))
        .unwrap();
    assert_eq!(export.entities[0].kind, "edge");
    assert_eq!(export.entities[0].source, Some(a.as_u64()));
    assert_eq!(export.entities[0].target, Some(b.as_u64()));
    assert!(export.verify(Some(&key().public_key())).is_ok());
}

#[test]
fn float_and_vector_values_survive_roundtrip_and_verify() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "M",
            PropertyMapBuilder::new()
                .insert("score", 0.3333333333333333f64)
                .insert(
                    "embedding",
                    crate::core::PropertyValue::vector(vec![0.1f32, 0.2, 0.3]),
                )
                .build(),
        )
        .unwrap();
    let export = db
        .audit_export(AuditScope::node(id), &key(), &ExportOptions::new("db"))
        .unwrap();
    let bytes = export.to_json_bytes().unwrap();
    let report = verify_json_bytes(&bytes, Some(&key().public_key())).unwrap();
    assert!(report.passed);
}

#[test]
fn float_token_roundtrip_nan_and_inf() {
    for v in [0.0f64, -0.0, 1.5, -2.25, f64::MAX, f64::MIN] {
        let t = super::model::f64_to_token(v);
        assert_eq!(
            super::model::token_to_f64(&t).unwrap().to_bits(),
            v.to_bits()
        );
    }
    assert!(
        super::model::token_to_f64(&super::model::f64_to_token(f64::NAN))
            .unwrap()
            .is_nan()
    );
    assert_eq!(super::model::token_to_f64("Infinity"), Some(f64::INFINITY));
    assert_eq!(
        super::model::token_to_f64("-Infinity"),
        Some(f64::NEG_INFINITY)
    );
    assert_eq!(super::model::token_to_f64("garbage"), None);
}

#[test]
fn render_chronology_contains_key_fields() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node("Person", PropertyMapBuilder::new().insert("n", "A").build())
        .unwrap();
    let export = db
        .audit_export(AuditScope::node(id), &key(), &ExportOptions::new("db"))
        .unwrap();
    let text = export.render_chronology();
    assert!(text.contains("Signed Audit Export"));
    assert!(text.contains("Chain root:"));
    assert!(text.contains("Version 1"));
}
