//! Regression tests for Issue #3388: `PersistenceConfig::default()` must not
//! silently enable index persistence rooted at a cwd-relative `data` directory.
//!
//! Before the fix, the default was `enabled: true` with `data_dir: "data"`, so
//! any database built from a default/builder config (without an explicit
//! `.persistence(...)`) would write index snapshots into `./data` on shutdown
//! and load whatever happened to be in `./data` on startup. Two unrelated
//! database instances — e.g. two tests, or two test *runs* — could therefore
//! observe each other's data, and a stale `./data` directory could
//! short-circuit WAL-replay tests (this caused a real CI flake during the
//! PR #3347 run).

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use tempfile::tempdir;

/// The default persistence config must be inert: no directory is read or
/// written unless the caller opts in with `enabled: true` and an explicit
/// `data_dir`.
#[test]
fn persistence_config_default_is_disabled() {
    assert!(
        !PersistenceConfig::default().enabled,
        "PersistenceConfig::default() must not enable persistence: the default \
         data_dir is cwd-relative, so an enabled default silently reads/writes \
         ./data (Issue #3388)"
    );
    assert!(
        !AletheiaDBConfig::default().persistence.enabled,
        "AletheiaDBConfig::default() (and thus builder configs without \
         .persistence(...)) must not enable persistence (Issue #3388)"
    );
    assert_eq!(
        PersistenceConfig::default().data_dir,
        std::path::PathBuf::from("data"),
        "the default data_dir is a cwd-relative placeholder that must stay \
         inert while enabled is false (Issue #3388); if this changes, revisit \
         the migration notes and the isolation test below"
    );
}

/// Two independent database instances created from default configs must not
/// leak data to each other through the *index-persistence* channel — the
/// shared cwd-relative `./data` directory the old enabled-by-default
/// `PersistenceConfig` wrote to and loaded from.
///
/// Scope: this proves the persistence channel is closed. It does NOT cover
/// the WAL channel: `WalConfig::default().wal_dir` is still the cwd-relative
/// `"aletheiadb/wal"`, a separate known footgun through which two default
/// configs sharing a cwd could still observe each other's data via WAL
/// replay — which is exactly why each instance below is given its own
/// isolated WAL directory. Fixing that default is a follow-up candidate.
///
/// Persistence is deliberately left at its default. Before the fix, instance
/// A's shutdown snapshot landed in `./data` and instance B loaded it on
/// startup (`load_on_startup` combined with the enabled default), so B saw
/// A's node.
///
/// Note: if the fix were reverted, the first failure here is clean (B
/// observes A's node), but the failed run leaves a stray `./data` directory
/// behind in the checkout.
#[test]
fn default_persistence_cannot_leak_between_instances() {
    let cwd_data = std::path::Path::new("data");
    let data_preexisted = cwd_data.exists();

    let dir_a = tempdir().expect("create tempdir for instance A");
    let dir_b = tempdir().expect("create tempdir for instance B");

    // Instance A: default config apart from an isolated WAL dir. Writes one
    // node, then drops (before the fix, drop triggered a final index snapshot
    // into the default cwd-relative ./data).
    {
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir_a.path().join("wal"))
                    .build(),
            )
            .build();
        let db = AletheiaDB::with_unified_config(config).expect("create instance A");
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("create node in instance A");
        assert_eq!(db.node_count(), 1);
    }

    // Instance B: an unrelated database, also on a default config (own WAL
    // dir, persistence untouched). It must start empty.
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir_b.path().join("wal"))
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create instance B");
    assert_eq!(
        db.node_count(),
        0,
        "an independent database with a default config must not observe \
         another instance's data (Issue #3388: default persistence leaked \
         state through the cwd-relative ./data directory)"
    );

    // The default config must not have materialized a cwd-relative ./data
    // directory as a side effect. Only assert when the directory did not
    // already exist, so a genuinely user-created ./data never fails this test.
    if !data_preexisted {
        assert!(
            !cwd_data.exists(),
            "a default config must not create a cwd-relative ./data directory \
             (Issue #3388)"
        );
    }
}
