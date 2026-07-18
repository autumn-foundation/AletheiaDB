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
    let key = SubjectKey::from_bytes(Zeroizing::new([0xAB; 32]));
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
    let sealed = envelope::seal(plaintext, "subj-1", 1, b"", cipher.as_ref()).unwrap();
    assert!(envelope::is_envelope(&sealed));
    let header = envelope::parse_header(&sealed).unwrap();
    assert_eq!(header.subject_id, "subj-1");
    assert_eq!(header.key_version, 1);
    let out = envelope::unseal(&sealed, b"", cipher.as_ref()).unwrap();
    assert_eq!(out, plaintext);
    // Plaintext must not appear verbatim in the sealed bytes.
    assert!(!sealed.windows(plaintext.len()).any(|w| w == plaintext));
}

#[test]
fn envelope_context_binds_write_site() {
    // An envelope sealed with one write-site context must not unseal under a
    // different context, even with the correct key — the context is folded into
    // the AAD, so a relocation within the same subject is a loud auth failure.
    let cipher = test_cipher(7);
    let sealed = envelope::seal(b"payload", "subj-C", 1, b"node:1|email", cipher.as_ref()).unwrap();
    // Correct context round-trips.
    let out = envelope::unseal(&sealed, b"node:1|email", cipher.as_ref()).unwrap();
    assert_eq!(out, b"payload");
    // Wrong context fails the AEAD auth check.
    let err = envelope::unseal(&sealed, b"node:2|email", cipher.as_ref()).unwrap_err();
    assert!(matches!(err, CryptoShredError::Crypto(_)));
    // Empty context (the value the seal did NOT use) also fails.
    assert!(matches!(
        envelope::unseal(&sealed, b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));
}

#[test]
fn envelope_malformed_bytes_error_not_panic() {
    let cipher = test_cipher(4);
    // Truncated (shorter than the fixed header).
    assert!(matches!(
        envelope::unseal(b"SUB", b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));
    assert!(matches!(
        envelope::parse_header(b"SUB"),
        Err(CryptoShredError::Crypto(_))
    ));
    // Wrong magic, header-length bytes but not "SUBJ".
    let wrong_magic = vec![0u8; envelope::ENVELOPE_HEADER_LEN + 8];
    assert!(matches!(
        envelope::unseal(&wrong_magic, b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));
    assert!(matches!(
        envelope::parse_header(&wrong_magic),
        Err(CryptoShredError::Crypto(_))
    ));
    // Random noise beginning with the magic but with a bogus subj_len that
    // overruns the buffer — must error, never panic.
    let mut bogus = Vec::new();
    bogus.extend_from_slice(envelope::ENVELOPE_MAGIC);
    bogus.push(envelope::ENVELOPE_FORMAT_VERSION);
    bogus.push(1); // alg id
    bogus.extend_from_slice(&1u32.to_le_bytes()); // key version
    bogus.extend_from_slice(&0xFFFFu16.to_le_bytes()); // subj_len = 65535 (overruns)
    assert!(matches!(
        envelope::unseal(&bogus, b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));
    assert!(matches!(
        envelope::parse_header(&bogus),
        Err(CryptoShredError::Crypto(_))
    ));
}

#[test]
fn envelope_aad_binds_subject_and_version() {
    // An envelope sealed for subj-A under key-version 1 must not decrypt when the
    // header is rewritten to claim a different subject/version — the AAD mismatch
    // is a loud auth failure, not fabricated plaintext (cross-entity swap guard).
    let cipher = test_cipher(2);
    let sealed = envelope::seal(b"secret", "subj-A", 1, b"", cipher.as_ref()).unwrap();

    // Tamper the subject id bytes in place (same length "subj-B"). The AEAD auth
    // path must fire (Crypto error), not merely "some error".
    let mut swapped = sealed.clone();
    let start = envelope::ENVELOPE_HEADER_LEN;
    swapped[start..start + 6].copy_from_slice(b"subj-B");
    assert!(matches!(
        envelope::unseal(&swapped, b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));

    // Tamper the key-version field.
    let mut kv = sealed.clone();
    kv[6] = 9;
    assert!(matches!(
        envelope::unseal(&kv, b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));

    // Cross-subject swap using subject B's ACTUALLY-DIFFERENT cipher (a distinct
    // key, not just a header-byte rewrite): B's cipher cannot unseal A's
    // envelope, and A's cipher cannot unseal an envelope B sealed for itself.
    let cipher_b = test_cipher(3);
    assert!(matches!(
        envelope::unseal(&sealed, b"", cipher_b.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));
    let sealed_b = envelope::seal(b"secret-b", "subj-B", 1, b"", cipher_b.as_ref()).unwrap();
    assert!(matches!(
        envelope::unseal(&sealed_b, b"", cipher.as_ref()),
        Err(CryptoShredError::Crypto(_))
    ));
}

#[test]
fn envelope_property_value_roundtrip() {
    let cipher = test_cipher(3);
    let value = PropertyValue::String(Arc::from("diagnosis: confidential"));
    let sealed = envelope::seal_property_value(&value, "subj-P", 1, b"", cipher.as_ref()).unwrap();
    let out = envelope::unseal_property_value(&sealed, b"", cipher.as_ref()).unwrap();
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
    let sealed =
        envelope::seal_property_value(&value, "subject-1", 1, b"", cipher.as_ref()).unwrap();
    let out = envelope::unseal_property_value(&sealed, b"", cipher.as_ref()).unwrap();
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
    let sealed = envelope::seal(b"personal-data", "gdpr-subject", 1, b"", cipher.as_ref()).unwrap();
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
    let sealed_b = envelope::seal(b"b-data", "B", 1, b"", cipher_b.as_ref()).unwrap();

    // Erase A.
    db.erase_subject("A").unwrap();

    // B's key still unwraps, byte-identical, and B's envelope still unseals.
    let key_b_after = *db.subject_key("B").unwrap().expose_bytes();
    assert_eq!(key_b_before, key_b_after);
    let out = envelope::unseal(&sealed_b, b"", cipher_b.as_ref()).unwrap();
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
fn erase_physically_removes_wrapped_key_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let kr_path = dir.path().join(keyring::KEYRING_FILENAME);
    {
        let db = enc_db(dir.path());
        db.designate_subject("gdpr", vec![DesignationTarget::WholeNode(1)])
            .unwrap();
        let before = keyring::load_keyring(&kr_path).unwrap();
        assert!(before.get("gdpr").unwrap().wrapped_key.is_some());
        db.erase_subject("gdpr").unwrap();
    }
    // Reopen COLD from disk: erasure must persist AND the wrapped DEK must be
    // physically gone.
    let db2 = enc_db(dir.path());
    assert!(matches!(
        db2.subject_key("gdpr"),
        Err(CryptoShredError::SubjectErased(_))
    ));
    let after = keyring::load_keyring(&kr_path).unwrap();
    assert!(
        after.get("gdpr").unwrap().wrapped_key.is_none(),
        "wrapped DEK must be physically removed, else key is recoverable"
    );
    assert!(db2.subject_key("gdpr").is_err());
}

#[test]
fn breadcrumb_crash_resume_erases_subject() {
    let dir = tempfile::tempdir().unwrap();
    let kr_path = dir.path().join(keyring::KEYRING_FILENAME);
    let sealed;
    let key_bytes;
    {
        let db = enc_db(dir.path());
        db.designate_subject("S", vec![DesignationTarget::WholeNode(1)])
            .unwrap();
        // Subject is active with a wrapped key on disk.
        assert!(db.crypto_shred.is_active("S"));
        // SEAL a value under the subject's key BEFORE the simulated crash, so we
        // can prove the prior envelope is unrecoverable after recovery.
        let key = db.subject_key("S").unwrap();
        key_bytes = *key.expose_bytes();
        let cipher = create_cipher(Algorithm::Aes256Gcm, &Zeroizing::new(key_bytes));
        sealed = envelope::seal(b"S-personal-data", "S", 1, b"", cipher.as_ref()).unwrap();
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
    drop(db2);

    // (i) A SECOND cold reopen still shows Erased (recovery was durable, not just
    // an in-memory flip).
    let db3 = enc_db(dir.path());
    assert!(db3.crypto_shred.is_erased("S"));
    // (ii) The wrapped_key is physically None in the reloaded keyring.
    let reloaded = keyring::load_keyring(&kr_path).unwrap();
    assert!(reloaded.get("S").unwrap().wrapped_key.is_none());
    // (iii) subject_key errs, so the prior envelope can never be unsealed.
    assert!(matches!(
        db3.subject_key("S"),
        Err(CryptoShredError::SubjectErased(_))
    ));
    // The old key bytes cannot help either: the point is the DEK is gone from the
    // durable store; the envelope still exists but is undecryptable via any live
    // path. (We deliberately do not "cheat" by reusing key_bytes — that only
    // works because the test captured them pre-crash; a real attacker post-erase
    // has no such copy.)
    let _ = &sealed;
    let _ = key_bytes;
}

#[test]
fn breadcrumb_resume_creates_tombstone_for_absent_subject() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = enc_db(dir.path());
        // Designate a DIFFERENT subject so the keyring file exists, but never
        // designate "ghost".
        db.designate_subject("other", vec![DesignationTarget::WholeNode(1)])
            .unwrap();
        drop(db);
    }

    // Write a breadcrumb naming a subject that was never designated.
    let bc_path = dir.path().join(keyring::BREADCRUMB_FILENAME);
    keyring::write_breadcrumb(&bc_path, "ghost").unwrap();

    // Reopen → fail-closed: a tombstone for "ghost" is minted (Erased), and the
    // breadcrumb is cleared.
    let db2 = enc_db(dir.path());
    assert!(db2.crypto_shred.is_erased("ghost"));
    assert!(matches!(
        db2.subject_key("ghost"),
        Err(CryptoShredError::SubjectErased(_))
    ));
    // The unrelated subject is untouched.
    assert!(db2.crypto_shred.is_active("other"));
    assert!(!bc_path.exists(), "breadcrumb must be cleared after resume");
}

#[test]
fn erase_mints_attestation_on_recovered_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = enc_db(dir.path());
        db.designate_subject("other", vec![DesignationTarget::WholeNode(1)])
            .unwrap();
        drop(db);
    }
    // Crash-recovered tombstone with attestation == None (minted by recovery, not
    // by an erase() call).
    let bc_path = dir.path().join(keyring::BREADCRUMB_FILENAME);
    keyring::write_breadcrumb(&bc_path, "ghost").unwrap();
    let db2 = enc_db(dir.path());
    assert!(db2.crypto_shred.is_erased("ghost"));

    // Calling erase_subject on the recovered tombstone must mint a valid SIGNED
    // attestation (it had none) that verifies with its embedded pubkey.
    let att = db2.erase_subject("ghost").unwrap();
    assert!(att.verify());
    assert_eq!(att.subject_id, "ghost");
    // Recovered orphan tombstone had no recorded designation → entity_count 0.
    assert_eq!(att.entity_count, 0);

    // A subsequent erase is now idempotent and returns the same attestation.
    let att2 = db2.erase_subject("ghost").unwrap();
    assert_eq!(att.signature, att2.signature);
    assert!(att2.verify());
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

// ── secret redaction (populated structures) ────────────────────────

#[test]
fn debug_of_populated_structures_leaks_no_secrets() {
    use keyring::{
        INITIAL_SUBJECT_KEY_VERSION, SubjectEntry, SubjectKeyring, SubjectState, WrappedKey,
    };

    // Distinctive, easily-greppable secret patterns.
    let wrapped_blob = vec![0x11u8; 48]; // hex "11" repeated
    let cached_key = SubjectKey::from_bytes(Zeroizing::new([0xCDu8; 32])); // hex "cd" repeated

    // A populated WrappedKey must not render its blob bytes.
    let wk = WrappedKey {
        key_version: INITIAL_SUBJECT_KEY_VERSION,
        wrapped: wrapped_blob.clone(),
    };
    let wk_dbg = format!("{wk:?}");
    assert!(
        !wk_dbg.contains("11, 11"),
        "WrappedKey debug leaked blob: {wk_dbg}"
    );
    assert!(
        !wk_dbg.contains("1111"),
        "WrappedKey debug leaked blob hex: {wk_dbg}"
    );

    // A populated SubjectKeyring (entry with wrapped blob + a cached DEK) must
    // render only counts / ids.
    let mut kr = SubjectKeyring::new();
    kr.upsert(SubjectEntry {
        subject_id: "redact-subject".to_string(),
        wrapped_key: Some(wk),
        designation: vec![DesignationTarget::WholeNode(1)],
        state: SubjectState::Active,
        created_at_micros: 1,
        erased_at_micros: None,
        attestation: None,
    });
    kr.cache_key("redact-subject", cached_key);
    let kr_dbg = format!("{kr:?}");
    // No wrapped-blob bytes and no cached-key hex.
    assert!(
        !kr_dbg.contains("11, 11"),
        "keyring debug leaked blob: {kr_dbg}"
    );
    assert!(
        !kr_dbg.contains("cd"),
        "keyring debug leaked key hex: {kr_dbg}"
    );
    assert!(
        !kr_dbg.contains("205, 205"),
        "keyring debug leaked key bytes: {kr_dbg}"
    );

    // A populated CryptoShredState (durable, with a wrapped blob + cached DEK on
    // disk) must render only its path / durability, never key material.
    let dir = tempfile::tempdir().unwrap();
    let db = enc_db(dir.path());
    db.designate_subject("state-subject", vec![DesignationTarget::WholeNode(1)])
        .unwrap();
    let _ = db.subject_key("state-subject").unwrap(); // populate the DEK cache
    let state_dbg = format!("{:?}", db.crypto_shred);
    assert!(state_dbg.contains("CryptoShredState"));
    // No key/blob material or entry contents should appear (the Debug renders
    // only the path + durability flag). We avoid substring-matching the random
    // DEK bytes (unknown) or the tempdir path; instead assert the structure does
    // not spill keyring internals.
    assert!(
        !state_dbg.contains("wrapped"),
        "state debug leaked wrap material: {state_dbg}"
    );
    assert!(
        !state_dbg.contains("SubjectEntry"),
        "state debug leaked entries: {state_dbg}"
    );
    assert!(
        !state_dbg.contains("cached_key"),
        "state debug leaked cache: {state_dbg}"
    );
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

// ── Slice 5: subject-keyring re-wrap (rewrap_keyring) focused unit tests ──

use super::CryptoShredState;
use super::subject::SUBJECT_KEY_LEN;
use super::wrap_aad;

/// Recover a subject's DEK bytes by unwrapping its on-disk wrapped blob under
/// `cipher` at `key_version` (test helper — the DEK is compared for equality
/// only, never logged).
fn recover_dek(
    state: &CryptoShredState,
    subject: &str,
    cipher: &dyn crate::encryption::Cipher,
    key_version: u32,
) -> Vec<u8> {
    let blob = state
        .subject_wrapped_dek_for_test(subject)
        .expect("subject has a wrapped DEK");
    let raw = cipher
        .decrypt(&blob, &wrap_aad(subject, key_version))
        .expect("wrapped DEK unwraps under the expected cipher + version");
    assert_eq!(raw.len(), SUBJECT_KEY_LEN);
    raw
}

#[test]
fn rewrap_keyring_advances_version_and_preserves_dek() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(keyring::KEYRING_FILENAME);
    let state = CryptoShredState::open(Some(path)).unwrap();
    let c_old = test_cipher(1);
    let c_new = test_cipher(2);
    let subj = SubjectId::new("s1").unwrap();
    state
        .designate(&subj, vec![DesignationTarget::WholeNode(1)], c_old.as_ref())
        .unwrap();

    // The DEK as wrapped under the OLD cipher at v1.
    let dek_before = recover_dek(&state, "s1", c_old.as_ref(), 1);
    assert!(state.keyring_has_stale_wrapped_entries(2));

    let n = state
        .rewrap_keyring(c_old.as_ref(), c_new.as_ref(), 2)
        .unwrap();
    assert_eq!(n, 1, "one entry re-wrapped");

    // Now the DEK unwraps under the NEW cipher at v2, byte-identical.
    let dek_after = recover_dek(&state, "s1", c_new.as_ref(), 2);
    assert_eq!(
        dek_before, dek_after,
        "the random DEK is preserved, only the wrapping changed"
    );
    assert!(
        !state.keyring_has_stale_wrapped_entries(2),
        "no stale entries after re-wrap"
    );
}

#[test]
fn rewrap_keyring_is_idempotent_no_op_on_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(keyring::KEYRING_FILENAME);
    let state = CryptoShredState::open(Some(path)).unwrap();
    let c_old = test_cipher(1);
    let c_new = test_cipher(2);
    let subj = SubjectId::new("s1").unwrap();
    state
        .designate(&subj, vec![DesignationTarget::WholeNode(1)], c_old.as_ref())
        .unwrap();

    assert_eq!(
        state
            .rewrap_keyring(c_old.as_ref(), c_new.as_ref(), 2)
            .unwrap(),
        1
    );
    // Second run to the SAME target: every entry is already at v2 -> 0 re-wrapped,
    // no error (the old cipher is never even consulted).
    let c_bogus = test_cipher(9);
    assert_eq!(
        state
            .rewrap_keyring(c_bogus.as_ref(), c_new.as_ref(), 2)
            .unwrap(),
        0,
        "idempotent re-run re-wraps zero entries"
    );
    // Value still recovers under the v2 cipher.
    recover_dek(&state, "s1", c_new.as_ref(), 2);
}

#[test]
fn rewrap_keyring_tolerates_mixed_versions_and_advances_only_stale() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(keyring::KEYRING_FILENAME);
    let state = CryptoShredState::open(Some(path)).unwrap();
    let c1 = test_cipher(1);
    let c2 = test_cipher(2);
    let c3 = test_cipher(3);

    // s1 designated under c1 (v1), then advanced to v2 under c2.
    state
        .designate(
            &SubjectId::new("s1").unwrap(),
            vec![DesignationTarget::WholeNode(1)],
            c1.as_ref(),
        )
        .unwrap();
    assert_eq!(
        state.rewrap_keyring(c1.as_ref(), c2.as_ref(), 2).unwrap(),
        1
    );
    // s2 designated AFTER the rotation, so it starts at v1 wrapped under c2.
    state
        .designate(
            &SubjectId::new("s2").unwrap(),
            vec![DesignationTarget::WholeNode(2)],
            c2.as_ref(),
        )
        .unwrap();

    // Mixed: s1@v2, s2@v1. Re-wrap to target v2 advances ONLY the stale s2.
    let n = state.rewrap_keyring(c2.as_ref(), c3.as_ref(), 2).unwrap();
    assert_eq!(n, 1, "only the stale (v1) entry is advanced");
    // s1 still at v2 under c2 (untouched); s2 now at v2 under c3.
    recover_dek(&state, "s1", c2.as_ref(), 2);
    recover_dek(&state, "s2", c3.as_ref(), 2);
}

#[test]
fn rewrap_keyring_skips_erased_subject_and_never_resurrects_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(keyring::KEYRING_FILENAME);
    let state = CryptoShredState::open(Some(path)).unwrap();
    let c_old = test_cipher(1);
    let c_new = test_cipher(2);
    state
        .designate(
            &SubjectId::new("gone").unwrap(),
            vec![DesignationTarget::WholeNode(1)],
            c_old.as_ref(),
        )
        .unwrap();
    state.erase(&SubjectId::new("gone").unwrap()).unwrap();
    assert!(
        state.subject_wrapped_dek_for_test("gone").is_none(),
        "erased: wrapped key gone"
    );

    // Re-wrap must skip the erased subject and never re-introduce a wrapped key.
    let n = state
        .rewrap_keyring(c_old.as_ref(), c_new.as_ref(), 2)
        .unwrap();
    assert_eq!(n, 0, "erased subject is not re-wrapped");
    assert!(
        state.subject_wrapped_dek_for_test("gone").is_none(),
        "erased key must stay gone"
    );
    assert!(state.is_erased("gone"));
}

#[test]
fn keyring_has_stale_wrapped_entries_false_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(keyring::KEYRING_FILENAME);
    let state = CryptoShredState::open(Some(path)).unwrap();
    assert!(
        !state.keyring_has_stale_wrapped_entries(2),
        "no subjects -> nothing stale"
    );
}

#[test]
fn rewrap_keyring_invalidates_memoized_wrap_cipher() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(keyring::KEYRING_FILENAME);
    let state = CryptoShredState::open(Some(path)).unwrap();
    let c_old = test_cipher(1);
    let c_new = test_cipher(2);
    state
        .designate(
            &SubjectId::new("s1").unwrap(),
            vec![DesignationTarget::WholeNode(1)],
            c_old.as_ref(),
        )
        .unwrap();

    // Populate the memoized wrap cipher (as a seal/unseal would).
    state.cached_wrap_cipher(|| Ok(test_cipher(1))).unwrap();
    assert!(
        state.wrap_cipher_cached_for_test(),
        "wrap cipher is memoized"
    );

    // Re-wrap invalidates the memoized cipher so the next seal/unseal rebuilds it.
    state
        .rewrap_keyring(c_old.as_ref(), c_new.as_ref(), 2)
        .unwrap();
    assert!(
        !state.wrap_cipher_cached_for_test(),
        "re-wrap must invalidate the memoized wrap cipher"
    );
}
