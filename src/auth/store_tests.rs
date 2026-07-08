//! Unit tests for the [`AuthStore`] (written first, TDD — Issue #3350).

use super::*;
use crate::auth::Role;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create tempdir")
}

// ---------------------------------------------------------------------------
// Key generation & format
// ---------------------------------------------------------------------------

#[test]
fn created_key_has_expected_format() {
    let store = AuthStore::new();
    let (principal, key) = store.create_key("ci-bot", Role::Reader).expect("create");

    assert!(
        key.starts_with(KEY_SCHEME_PREFIX),
        "key must start with the scheme prefix, got: {}",
        &key[..key.len().min(16)]
    );
    let random_part = &key[KEY_SCHEME_PREFIX.len()..];
    // 32 bytes base64url without padding => 43 characters.
    assert_eq!(random_part.len(), 43, "random part must be 43 chars");
    assert!(
        random_part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "random part must be base64url"
    );

    // Prefix: scheme + first 8 chars of random part.
    assert_eq!(
        principal.key_prefix,
        format!("{}{}", KEY_SCHEME_PREFIX, &random_part[..8])
    );
    assert!(key.starts_with(&principal.key_prefix));

    assert_eq!(principal.name, "ci-bot");
    assert_eq!(principal.role, Role::Reader);
    assert!(!principal.id.is_empty());
    assert!(!principal.created_at.is_empty());
}

#[test]
fn created_keys_are_unique() {
    let store = AuthStore::new();
    let mut keys = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    for i in 0..64 {
        let (principal, key) = store
            .create_key(&format!("key-{i}"), Role::Reader)
            .expect("create");
        assert!(keys.insert(key.to_string()), "duplicate key generated");
        assert!(ids.insert(principal.id.clone()), "duplicate id generated");
    }
}

// ---------------------------------------------------------------------------
// Verify / revoke
// ---------------------------------------------------------------------------

#[test]
fn verify_roundtrip_accepts_created_key() {
    let store = AuthStore::new();
    let (principal, key) = store.create_key("svc", Role::Writer).expect("create");

    let verified = store.verify(&key).expect("key must verify");
    assert_eq!(verified.id, principal.id);
    assert_eq!(verified.role, Role::Writer);
    assert_eq!(verified.name, "svc");
}

#[test]
fn verify_rejects_wrong_key() {
    let store = AuthStore::new();
    let (_principal, key) = store.create_key("svc", Role::Writer).expect("create");

    assert!(store.verify("aletheia_sk_not_a_real_key").is_none());
    assert!(store.verify("").is_none());
    // A key differing in the last character must not verify.
    let mut tampered = key.to_string();
    tampered.pop();
    tampered.push(if key.ends_with('A') { 'B' } else { 'A' });
    assert!(store.verify(&tampered).is_none());
}

#[test]
fn revoke_takes_effect_immediately() {
    let store = AuthStore::new();
    let (principal, key) = store.create_key("victim", Role::Reader).expect("create");

    assert!(store.verify(&key).is_some());
    assert!(store.revoke(&principal.id).expect("revoke"));
    assert!(store.verify(&key).is_none(), "revoked key must not verify");
    // Second revoke reports "was not present".
    assert!(!store.revoke(&principal.id).expect("re-revoke"));
}

#[test]
fn revoke_unknown_id_returns_false() {
    let store = AuthStore::new();
    assert!(!store.revoke("no-such-id").expect("revoke"));
}

// ---------------------------------------------------------------------------
// Listing is masked by construction
// ---------------------------------------------------------------------------

#[test]
fn list_returns_principals_without_key_material() {
    let store = AuthStore::new();
    let (_p1, key1) = store.create_key("alpha", Role::Admin).expect("create");
    let (_p2, key2) = store.create_key("beta", Role::Reader).expect("create");

    let listed = store.list();
    assert_eq!(listed.len(), 2);
    let json = serde_json::to_string(&listed).expect("serialize principals");
    assert!(!json.contains(&*key1), "list leaked plaintext key");
    assert!(!json.contains(&*key2), "list leaked plaintext key");
    // Prefixes are present for masked identification.
    assert!(listed.iter().all(|p| p.key_prefix.len() < key1.len()));
    assert!(listed.iter().any(|p| p.name == "alpha"));
    assert!(listed.iter().any(|p| p.name == "beta"));
}

// ---------------------------------------------------------------------------
// Bootstrap keys
// ---------------------------------------------------------------------------

#[test]
fn bootstrap_key_verifies_with_configured_role() {
    let store = AuthStore::new();
    let principal = store
        .insert_bootstrap_key("bootstrap-admin", Role::Admin, "my-plaintext-admin-key")
        .expect("bootstrap");
    assert_eq!(principal.name, "bootstrap-admin");

    let verified = store.verify("my-plaintext-admin-key").expect("verify");
    assert_eq!(verified.role, Role::Admin);
    assert!(!store.is_empty());
}

#[test]
fn bootstrap_key_is_idempotent_for_same_plaintext() {
    let store = AuthStore::new();
    let p1 = store
        .insert_bootstrap_key("bootstrap-admin", Role::Admin, "same-key")
        .expect("first");
    let p2 = store
        .insert_bootstrap_key("bootstrap-admin", Role::Admin, "same-key")
        .expect("second");
    assert_eq!(p1.id, p2.id, "re-inserting same key must not duplicate");
    assert_eq!(store.list().len(), 1);
}

#[test]
fn bootstrap_key_rejects_empty_plaintext() {
    let store = AuthStore::new();
    assert!(
        store
            .insert_bootstrap_key("bootstrap-admin", Role::Admin, "")
            .is_err()
    );
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[test]
fn persistence_roundtrip_preserves_keys_but_not_plaintext() {
    let dir = tempdir();
    let path = dir.path().join("auth").join("keys.json");

    let (principal, key) = {
        let store = AuthStore::with_persistence(&path).expect("create store");
        let (p, k) = store.create_key("durable", Role::Writer).expect("create");
        (p, k.to_string())
    };

    // The file must exist and must NOT contain the plaintext key.
    let contents = std::fs::read_to_string(&path).expect("read persisted store");
    assert!(
        !contents.contains(&key),
        "persisted auth store leaked plaintext key"
    );
    // But it should contain the masked metadata.
    assert!(contents.contains("durable"));
    assert!(contents.contains(&principal.id));

    // Reload: the key still verifies.
    let reloaded = AuthStore::with_persistence(&path).expect("reload store");
    let verified = reloaded.verify(&key).expect("reloaded key verifies");
    assert_eq!(verified.id, principal.id);
    assert_eq!(verified.role, Role::Writer);
    assert_eq!(verified.name, "durable");
}

#[test]
fn persistence_revoke_survives_reload() {
    let dir = tempdir();
    let path = dir.path().join("keys.json");

    let store = AuthStore::with_persistence(&path).expect("create store");
    let (p1, key1) = store.create_key("keep", Role::Reader).expect("create");
    let (p2, key2) = store.create_key("drop", Role::Reader).expect("create");
    assert!(store.revoke(&p2.id).expect("revoke"));

    let reloaded = AuthStore::with_persistence(&path).expect("reload");
    assert!(reloaded.verify(&key1).is_some());
    assert!(reloaded.verify(&key2).is_none());
    assert_eq!(reloaded.list().len(), 1);
    assert_eq!(reloaded.list()[0].id, p1.id);
}

#[cfg(unix)]
#[test]
fn persisted_file_has_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir();
    let path = dir.path().join("keys.json");
    let store = AuthStore::with_persistence(&path).expect("create store");
    store
        .create_key("perm-check", Role::Reader)
        .expect("create");

    let mode = std::fs::metadata(&path)
        .expect("stat persisted store")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "auth store file must be 0600");
}

#[test]
fn with_persistence_on_missing_file_starts_empty() {
    let dir = tempdir();
    let path = dir.path().join("does-not-exist.json");
    let store = AuthStore::with_persistence(&path).expect("create");
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn with_persistence_rejects_corrupt_file() {
    let dir = tempdir();
    let path = dir.path().join("keys.json");
    std::fs::write(&path, b"this is not json").expect("write corrupt file");
    assert!(AuthStore::with_persistence(&path).is_err());
}

#[test]
fn bootstrap_keys_are_not_persisted() {
    let dir = tempdir();
    let path = dir.path().join("keys.json");

    {
        let store = AuthStore::with_persistence(&path).expect("create");
        store
            .insert_bootstrap_key("bootstrap-admin", Role::Admin, "env-supplied-key")
            .expect("bootstrap");
        // Force a save by creating a persisted key too.
        store.create_key("normal", Role::Reader).expect("create");
    }

    let contents = std::fs::read_to_string(&path).expect("read");
    assert!(
        !contents.contains("bootstrap-admin"),
        "bootstrap (env-supplied) keys must stay memory-only"
    );

    let reloaded = AuthStore::with_persistence(&path).expect("reload");
    assert!(
        reloaded.verify("env-supplied-key").is_none(),
        "bootstrap key must not survive a reload on its own"
    );
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

#[test]
fn create_key_rejects_empty_name() {
    let store = AuthStore::new();
    assert!(store.create_key("", Role::Reader).is_err());
    assert!(store.create_key("   ", Role::Reader).is_err());
}

#[test]
fn debug_output_contains_no_digests() {
    let store = AuthStore::new();
    let (_p, key) = store.create_key("dbg", Role::Reader).expect("create");
    let debug = format!("{store:?}");
    assert!(!debug.contains(&*key), "Debug leaked plaintext");
    // The digest hex must not appear either — Debug is counts-only.
    assert!(
        !debug.contains("digest"),
        "Debug should not dump digest material"
    );
}
