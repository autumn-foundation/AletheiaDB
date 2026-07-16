//! Integration coverage for the durable encryption-state authority
//! (`{data_dir}/encryption.state`) as consumed by `AletheiaDB::open()`
//! (Issue #3616).
//!
//! These exercise the PRODUCTION-wired READ + precedence + fail-closed path in
//! `with_unified_config`. The authority file is planted directly on disk (the
//! `encryption enable` migration engine that *writes* it lands in a follow-up
//! commit on this PR); the round-trip/crash-safety of the writer itself is
//! covered by the unit tests in `src/db/encryption_state.rs`.

use aletheiadb::{AletheiaDB, PropertyMapBuilder};

/// The authority file lives under the persistence data dir, which
/// `durable_config_for_data_dir` roots at `{path}/indexes`.
fn authority_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("indexes").join("encryption.state")
}

#[test]
fn absent_authority_opens_as_before() {
    // Backward compatibility: a durable database with NO authority file opens
    // and round-trips exactly as today.
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap()
    };
    let db = AletheiaDB::open(dir.path()).unwrap();
    let node = db.get_node(id).unwrap();
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
    assert!(!authority_path(dir.path()).exists());
}

#[test]
fn enabled_authority_with_missing_key_refuses_open() {
    // Precedence + fail-closed: the same plaintext data dir that opens cleanly
    // with no authority REFUSES once an `enabled` authority naming an
    // unresolvable key source is planted — the authority forced encryption ON,
    // and a missing key never degrades to a silent plaintext open.
    let dir = tempfile::tempdir().unwrap();

    // First open establishes the durable directory tree (incl. `indexes/`).
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
    }
    // Sanity: it reopens fine while no authority exists.
    drop(AletheiaDB::open(dir.path()).unwrap());

    // Plant an authority that says "encrypted under env var $NOPE_UNSET_MEK"
    // (guaranteed unset), so the encryption manager cannot source the MEK.
    std::fs::write(
        authority_path(dir.path()),
        b"version=1\nenabled=true\nkey_source_kind=env\nkey_source_value=ALETHEIADB_NOPE_UNSET_MEK_3616\n",
    )
    .unwrap();

    let result = AletheiaDB::open(dir.path());
    assert!(
        result.is_err(),
        "open() must refuse (fail-closed) when the authority is enabled but the key source is \
         unresolvable, never fall back to plaintext"
    );
}

#[test]
fn corrupt_authority_refuses_open() {
    // A present-but-corrupt authority aborts startup loudly (never guessed as
    // "not enabled").
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
    }
    std::fs::write(authority_path(dir.path()), b"garbage-not-key-value\n").unwrap();

    assert!(
        AletheiaDB::open(dir.path()).is_err(),
        "open() must fail closed on a corrupt encryption-state authority"
    );
}

#[test]
fn disabled_authority_opens_plaintext() {
    // An authority that records `enabled=false` agrees with the plaintext
    // durable config and opens normally (exercises the read + no-op override).
    let dir = tempfile::tempdir().unwrap();
    let id = {
        let db = AletheiaDB::open(dir.path()).unwrap();
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .unwrap()
    };
    std::fs::write(authority_path(dir.path()), b"version=1\nenabled=false\n").unwrap();

    let db = AletheiaDB::open(dir.path()).unwrap();
    assert_eq!(
        db.get_node(id)
            .unwrap()
            .properties
            .get("name")
            .and_then(|v| v.as_str()),
        Some("Bob")
    );
}
