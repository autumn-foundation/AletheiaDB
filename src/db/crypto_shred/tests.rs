//! Crypto-shred foundation tests (Issue #3359, slice PR-1a).
//!
//! Covers the cryptographic core in isolation: envelope codec, designation
//! rule, durable keyring lifecycle, key destruction (AC1), blast radius (AC6),
//! fail-closed load, breadcrumb crash-resume, attestation signing, secret
//! redaction, and the ephemeral (in-memory) path.

use std::path::Path;
use std::sync::Arc;

use zeroize::Zeroizing;

use crate::config::{AletheiaDBConfig, WalConfigBuilder};
use crate::core::property::PropertyValue;
use crate::db::AletheiaDB;
use crate::encryption::config::EncryptionConfig;
use crate::encryption::{Algorithm, create_cipher};
use crate::storage::index_persistence::PersistenceConfig;

use super::designation::DesignationTarget;
use super::error::CryptoShredError;
use super::subject::{SubjectId, SubjectKey};
use super::{attestation, envelope, keyring};

// ── helpers ────────────────────────────────────────────────────────

fn test_cipher(byte: u8) -> Box<dyn crate::encryption::Cipher> {
    create_cipher(Algorithm::Aes256Gcm, &Zeroizing::new([byte; 32]))
}

/// Build a persistent, encrypted config rooted at `root` (keyring lands at
/// `root/subject_keyring.dat`).
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

/// In-memory (persistence-disabled) but encryption-enabled config — the
/// ephemeral crypto-shred path.
fn ephemeral_enc_db(key_dir: &Path) -> AletheiaDB {
    let key_file = key_dir.join("mek.key");
    crate::encryption::FileKeyProvider::generate_key_file(&key_file).unwrap();
    let config = AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().wal_dir(key_dir.join("wal")).build())
        .encryption(EncryptionConfig::file_based(&key_file))
        .build();
    AletheiaDB::with_unified_config(config).unwrap()
}

// ── subject id + key ───────────────────────────────────────────────

#[test]
fn subject_id_validation() {
    assert!(SubjectId::new("user-42").is_ok());
    assert!(matches!(
        SubjectId::new(""),
        Err(CryptoShredError::InvalidArgument(_))
    ));
    assert!(matches!(
        SubjectId::new("a\nb"),
        Err(CryptoShredError::InvalidArgument(_))
    ));
    let too_long = "x".repeat(super::subject::MAX_SUBJECT_ID_LEN + 1);
    assert!(matches!(
        SubjectId::new(too_long),
        Err(CryptoShredError::InvalidArgument(_))
    ));
}

#[test]
fn subject_key_debug_redacts() {
    let key = SubjectKey::from_bytes([0xAB; 32]);
    let dbg = format!("{key:?}");
    assert_eq!(dbg, "SubjectKey(<redacted>)");
    // No key byte / hex leaks.
    assert!(!dbg.contains("ab"));
    assert!(!dbg.contains("171"));
}

// ── envelope codec ─────────────────────────────────────────────────

#[test]
fn envelope_seal_unseal_roundtrip() {
    let cipher = test_cipher(1);
    let plaintext = b"the-quick-brown-fox";
    let sealed = envelope::seal(plaintext, "subj-1", 1, cipher.as_ref()).unwrap();
    assert!(envelope::is_envelope(&sealed));
    let header = envelope::parse_header(&sealed).unwrap();
    assert_eq!(header.subject_id, "subj-1");
    assert_eq!(header.key_version, 1);
    let out = envelope::unseal(&sealed, cipher.as_ref()).unwrap();
    assert_eq!(out, plaintext);
    // Plaintext must not appear verbatim in the sealed bytes.
    assert!(!sealed.windows(plaintext.len()).any(|w| w == plaintext));
}

#[test]
fn envelope_aad_binds_subject_and_version() {
    // An envelope sealed for subj-A under key-version 1 must not decrypt when the
    // header is rewritten to claim a different subject/version — the AAD mismatch
    // is a loud auth failure, not fabricated plaintext (cross-entity swap guard).
    let cipher = test_cipher(2);
    let sealed = envelope::seal(b"secret", "subj-A", 1, cipher.as_ref()).unwrap();

    // Tamper the subject id bytes in place (same length "subj-B").
    let mut swapped = sealed.clone();
    let start = envelope::ENVELOPE_HEADER_LEN;
    swapped[start..start + 6].copy_from_slice(b"subj-B");
    assert!(envelope::unseal(&swapped, cipher.as_ref()).is_err());

    // Tamper the key-version field.
    let mut kv = sealed.clone();
    kv[6] = 9;
    assert!(envelope::unseal(&kv, cipher.as_ref()).is_err());
}

#[test]
fn envelope_property_value_roundtrip() {
    let cipher = test_cipher(3);
    let value = PropertyValue::String(Arc::from("diagnosis: confidential"));
    let sealed = envelope::seal_property_value(&value, "subj-P", 1, cipher.as_ref()).unwrap();
    let out = envelope::unseal_property_value(&sealed, cipher.as_ref()).unwrap();
    assert_eq!(out, value);
}

// ── designation rule ───────────────────────────────────────────────

#[test]
fn designation_reserved_key_exempt_whole_node() {
    let t = DesignationTarget::WholeNode(7);
    assert!(t.should_seal_key(true, 7, "name"));
    // Engine-reserved keys are never sealed.
    assert!(!t.should_seal_key(true, 7, "__aletheia_ns"));
    // Wrong entity / wrong kind does not match.
    assert!(!t.should_seal_key(true, 8, "name"));
    assert!(!t.should_seal_key(false, 7, "name"));
}

#[test]
fn designation_property_scoped() {
    let t = DesignationTarget::NodeProperties(5, vec!["email".to_string(), "ssn".to_string()]);
    assert!(t.should_seal_key(true, 5, "email"));
    assert!(t.should_seal_key(true, 5, "ssn"));
    // Not-listed key is not sealed.
    assert!(!t.should_seal_key(true, 5, "name"));
    // Reserved key never sealed even if it were listed.
    let reserved = DesignationTarget::NodeProperties(5, vec!["__aletheia_x".to_string()]);
    assert!(!reserved.should_seal_key(true, 5, "__aletheia_x"));
}

// ── keyring lifecycle via the DB API ───────────────────────────────

#[test]
fn keyring_roundtrip_seal_unseal() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.designate_subject("subject-1", vec![DesignationTarget::WholeNode(1)])
        .unwrap();

    // subject_key unwraps to a usable DEK; seal a value and unseal it back.
    let key = db.subject_key("subject-1").unwrap();
    let cipher = create_cipher(Algorithm::Aes256Gcm, &Zeroizing::new(*key.expose_bytes()));
    let value = PropertyValue::Int(4242);
    let sealed = envelope::seal_property_value(&value, "subject-1", 1, cipher.as_ref()).unwrap();
    let out = envelope::unseal_property_value(&sealed, cipher.as_ref()).unwrap();
    assert_eq!(out, value);
}

#[test]
fn erase_destroys_key_ac1() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.designate_subject("gdpr-subject", vec![DesignationTarget::WholeNode(1)])
        .unwrap();

    // Seal a value under the subject key while active.
    let key = db.subject_key("gdpr-subject").unwrap();
    let cipher = create_cipher(Algorithm::Aes256Gcm, &Zeroizing::new(*key.expose_bytes()));
    let sealed = envelope::seal(b"personal-data", "gdpr-subject", 1, cipher.as_ref()).unwrap();
    drop(key);
    drop(cipher);

    // Erase → key destroyed.
    let attestation = db.erase_subject("gdpr-subject").unwrap();
    assert!(attestation.verify());

    // subject_key now errors (key gone), and no cipher can be built to unseal.
    assert!(matches!(
        db.subject_key("gdpr-subject"),
        Err(CryptoShredError::SubjectErased(_))
    ));

    // The previously-sealed envelope is now permanently unrecoverable: even the
    // MEK cannot regenerate the random DEK. (Prove there is no path to a key.)
    assert!(db.subject_key("gdpr-subject").is_err());
    // Sanity: the envelope bytes still exist but decrypt only under the gone key.
    assert!(envelope::is_envelope(&sealed));
}

#[test]
fn blast_radius_ac6() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.designate_subject("A", vec![DesignationTarget::WholeNode(1)])
        .unwrap();
    db.designate_subject("B", vec![DesignationTarget::WholeNode(2)])
        .unwrap();

    let key_b_before = *db.subject_key("B").unwrap().expose_bytes();
    let cipher_b = create_cipher(Algorithm::Aes256Gcm, &Zeroizing::new(key_b_before));
    let sealed_b = envelope::seal(b"b-data", "B", 1, cipher_b.as_ref()).unwrap();

    // Erase A.
    db.erase_subject("A").unwrap();

    // B's key still unwraps, byte-identical, and B's envelope still unseals.
    let key_b_after = *db.subject_key("B").unwrap().expose_bytes();
    assert_eq!(key_b_before, key_b_after);
    let out = envelope::unseal(&sealed_b, cipher_b.as_ref()).unwrap();
    assert_eq!(out, b"b-data");
}

#[test]
fn undesignated_erase_is_failed_precondition() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    let err = db.erase_subject("never-designated").unwrap_err();
    assert!(matches!(err, CryptoShredError::NotDesignated(_)));
    assert_eq!(err.code(), "FAILED_PRECONDITION");
}

#[test]
fn re_erase_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.designate_subject("S", vec![DesignationTarget::WholeNode(1)])
        .unwrap();
    let a1 = db.erase_subject("S").unwrap();
    let a2 = db.erase_subject("S").unwrap();
    assert_eq!(a1.subject_id, a2.subject_id);
    assert_eq!(a1.timestamp_micros, a2.timestamp_micros);
    assert_eq!(a1.signature, a2.signature);
    assert!(a2.verify());
}

#[test]
fn fail_closed_on_corrupt_keyring() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = enc_db(dir.path());
        db.designate_subject("S", vec![DesignationTarget::WholeNode(1)])
            .unwrap();
        drop(db);
    }
    // Corrupt the keyring file (flip a byte → CRC mismatch).
    let keyring_path = dir.path().join(keyring::KEYRING_FILENAME);
    let mut bytes = std::fs::read(&keyring_path).unwrap();
    assert!(!bytes.is_empty());
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    std::fs::write(&keyring_path, &bytes).unwrap();

    // Reopening must FAIL closed (not silently start with an empty registry).
    let result = AletheiaDB::with_unified_config(enc_config(dir.path()));
    assert!(result.is_err(), "corrupt keyring must fail closed on open");
}

#[test]
fn breadcrumb_crash_resume_erases_subject() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = enc_db(dir.path());
        db.designate_subject("S", vec![DesignationTarget::WholeNode(1)])
            .unwrap();
        // Subject is active with a wrapped key on disk.
        assert!(db.crypto_shred.is_active("S"));
        drop(db);
    }

    // Simulate a crash AFTER the breadcrumb was written but BEFORE the keyring
    // rewrite: write the breadcrumb naming "S", leaving the keyring untouched
    // (S still active with its wrapped key).
    let bc_path = dir.path().join(keyring::BREADCRUMB_FILENAME);
    keyring::write_breadcrumb(&bc_path, "S").unwrap();

    // Reopen → recovery must force S erased and clear the breadcrumb.
    let db2 = enc_db(dir.path());
    assert!(db2.crypto_shred.is_erased("S"));
    assert!(matches!(
        db2.subject_key("S"),
        Err(CryptoShredError::SubjectErased(_))
    ));
    assert!(!bc_path.exists(), "breadcrumb must be cleared after resume");
}

// ── attestation ────────────────────────────────────────────────────

#[test]
fn attestation_signature_verifies_and_carries_no_content() {
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.designate_subject(
        "attest-subject",
        vec![
            DesignationTarget::WholeNode(1),
            DesignationTarget::WholeNode(2),
        ],
    )
    .unwrap();
    let att = db.erase_subject("attest-subject").unwrap();

    assert!(att.verify());
    assert_eq!(att.subject_id, "attest-subject");
    assert_eq!(att.entity_count, 2);
    assert!(att.timestamp_micros > 0);

    // Verifies against the db's advertised public key too.
    let pk = db.erasure_attestation_public_key();
    assert_eq!(pk.to_hex(), att.signer_public_key.to_hex());

    // A record round-trips and re-verifies.
    let record = att.to_record();
    let rebuilt = attestation::ErasureAttestation::from_record(&record).unwrap();
    assert!(rebuilt.verify());
}

// ── ephemeral (in-memory) path ─────────────────────────────────────

#[test]
fn ephemeral_designate_and_erase() {
    let dir = tempfile::tempdir().unwrap();
    let db = ephemeral_enc_db(dir.path());
    // Persistence off → keyring is in-memory only; must not panic.
    assert!(!db.crypto_shred.is_durable());

    db.designate_subject("mem-subject", vec![DesignationTarget::WholeEdge(9)])
        .unwrap();
    let key = db.subject_key("mem-subject").unwrap();
    assert_eq!(key.expose_bytes().len(), 32);

    let att = db.erase_subject("mem-subject").unwrap();
    assert!(att.verify());
    assert!(matches!(
        db.subject_key("mem-subject"),
        Err(CryptoShredError::SubjectErased(_))
    ));

    // No keyring file was ever created (no data dir).
    assert!(!dir.path().join(keyring::KEYRING_FILENAME).exists());
}

#[test]
fn designate_without_encryption_is_failed_precondition() {
    // Plain in-memory DB with NO encryption configured.
    let db = AletheiaDB::new().unwrap();
    let err = db
        .designate_subject("x", vec![DesignationTarget::WholeNode(1)])
        .unwrap_err();
    assert!(matches!(err, CryptoShredError::EncryptionNotConfigured));
    assert_eq!(err.code(), "FAILED_PRECONDITION");
}
