//! End-to-end tests for the `aletheia keys rotate` verb and the `aletheia
//! encryption` subcommand group (Issue #490 -- CLI wiring for the shipped key
//! rotation engine, Issue #488).
//!
//! Each test invokes the real built binary via `env!("CARGO_BIN_EXE_aletheia")`
//! in its own process, mirroring `tests/cli_keys.rs`.
//!
//! Security-critical invariant asserted throughout: raw key hex must NEVER
//! appear in stdout/stderr, in any encoding an error message might use.
//!
//! ## Architectural note (why no successful-rotation CLI test)
//!
//! The shipped rotation engine (`AletheiaDB::rotate_index_keys`) is *index
//! layer only* and hard-refuses whenever any OTHER at-rest layer (WAL, cold,
//! checkpoint) is encrypted under the same master key -- rotating the index
//! alone to a new MEK would strand those layers (`rotation.rs` P0.1). Because
//! AletheiaDB's config encrypts *uniformly* (enabling encryption encrypts the
//! WAL too; there is no config knob for index-only encryption), a normally
//! opened encrypted database ALWAYS has an encrypted WAL. Therefore
//! `keys rotate --new-key` against any config-encrypted database correctly
//! REFUSES with the cross-layer guard -- and that honest refusal is what these
//! tests assert. A CLI-reachable successful rotation must wait on the
//! documented full-MEK (all-layer) rotation follow-up.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// Result of running the CLI binary once.
struct CliRun {
    code: i32,
    stdout: String,
    stderr: String,
}

impl CliRun {
    /// Combined stdout+stderr, lowercased, for message assertions.
    fn combined_lower(&self) -> String {
        format!("{}{}", self.stdout, self.stderr).to_lowercase()
    }
}

/// Run the `aletheia` binary with `args` and an explicit environment: every
/// pair in `env` is set, and any ambient `ALETHEIADB_CONFIG` / `ALETHEIADB_DATA_DIR`
/// not overridden by `env` is removed so the child sees a deterministic config.
fn run_env(args: &[&str], env: &[(&str, &str)]) -> CliRun {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aletheia"));
    cmd.args(args);
    cmd.env_remove("ALETHEIADB_CONFIG");
    cmd.env_remove("ALETHEIADB_DATA_DIR");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("failed to spawn aletheia binary");
    CliRun {
        code: output.status.code().expect("process terminated by signal"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Run with no ambient database config.
fn run(args: &[&str]) -> CliRun {
    run_env(args, &[])
}

/// Assert no raw key hex from any of `key_files` leaks into the output.
fn assert_no_key_leak(r: &CliRun, key_files: &[&Path]) {
    for kf in key_files {
        let raw = std::fs::read_to_string(kf).unwrap_or_default();
        let hex = raw.trim().to_string();
        if hex.is_empty() {
            continue;
        }
        for out in [&r.stdout, &r.stderr] {
            assert!(
                !out.contains(&hex),
                "output must NEVER contain raw key hex from {kf:?}; got={out:?}"
            );
        }
    }
}

// ============================================================================
// keys rotate -- behavior WITHOUT a configured database (not-configured paths)
// ============================================================================

#[test]
fn keys_rotate_bare_requires_a_key_source_or_action() {
    // Bare `keys rotate` (no --new-key / --status / --resume / --cancel) is a
    // usage error -- NOT the old "not yet available" stub, and NOT the generic
    // "unknown subcommand" fallback.
    let r = run(&["keys", "rotate"]);
    assert_ne!(r.code, 0, "bare keys rotate must exit non-zero");
    let c = r.combined_lower();
    assert!(
        c.contains("rotate") && (c.contains("--new-key") || c.contains("usage")),
        "bare keys rotate must show usage naming a key source; got={c:?}"
    );
    assert!(
        !c.contains("not yet available"),
        "keys rotate must no longer report the deferred stub; got={c:?}"
    );
    assert!(
        !c.contains("unknown keys subcommand"),
        "keys rotate must not use the generic unknown-subcommand message; got={c:?}"
    );
}

#[test]
fn keys_rotate_status_without_encryption_reports_not_configured() {
    // No ambient DB -> ephemeral, no persistence/encryption -> a clear
    // not-configured error, non-zero, no panic.
    let r = run(&["keys", "rotate", "--status"]);
    assert_ne!(r.code, 0, "rotate --status on a plaintext DB must fail");
    let c = r.combined_lower();
    assert!(
        c.contains("encryption") || c.contains("persistence") || c.contains("not configured"),
        "must explain rotation needs an encrypted persistent DB; got={c:?}"
    );
    assert!(!c.contains("panicked"), "must not panic; got={c:?}");
}

#[test]
fn keys_rotate_start_without_encryption_reports_not_configured() {
    let dir = TempDir::new().unwrap();
    let key = dir.path().join("new.key");
    let g = run(&["keys", "generate", "--output", key.to_str().unwrap()]);
    assert_eq!(g.code, 0, "generate must succeed");

    let r = run(&["keys", "rotate", "--new-key", key.to_str().unwrap()]);
    assert_ne!(
        r.code, 0,
        "rotate --new-key on a plaintext DB must fail (nothing to rotate)"
    );
    let c = r.combined_lower();
    assert!(
        c.contains("encryption") || c.contains("persistence") || c.contains("not configured"),
        "must explain rotation needs an encrypted persistent DB; got={c:?}"
    );
    assert!(!c.contains("panicked"), "must not panic; got={c:?}");
    assert_no_key_leak(&r, &[key.as_path()]);
}

// ============================================================================
// encryption -- routing, disabled status, and honest enable/disable errors
// ============================================================================

#[test]
fn encryption_status_disabled_is_informational() {
    let r = run(&["encryption", "status"]);
    assert_eq!(
        r.code, 0,
        "encryption status with no config must be informational (exit 0); stderr={:?}",
        r.stderr
    );
    let lower = r.stdout.to_lowercase();
    assert!(
        lower.contains("disabled") || lower.contains("not configured"),
        "status must state encryption is not configured; stdout={:?}",
        r.stdout
    );
    // Per-layer table names the at-rest layers even when disabled.
    assert!(
        lower.contains("wal") && lower.contains("index"),
        "status must show a per-layer table; stdout={:?}",
        r.stdout
    );
}

#[test]
fn encryption_enable_reports_missing_migration_engine() {
    let r = run(&["encryption", "enable"]);
    assert_ne!(
        r.code, 0,
        "encryption enable must exit non-zero (no engine)"
    );
    let c = r.combined_lower();
    assert!(
        c.contains("migration")
            && (c.contains("not") && c.contains("implement") || c.contains("not supported")),
        "enable must honestly name the missing migration engine; got={c:?}"
    );
    assert!(!c.contains("panicked"), "must not panic; got={c:?}");
}

#[test]
fn encryption_disable_reports_missing_migration_engine() {
    let r = run(&["encryption", "disable"]);
    assert_ne!(
        r.code, 0,
        "encryption disable must exit non-zero (no engine)"
    );
    let c = r.combined_lower();
    assert!(
        c.contains("migration")
            && (c.contains("not") && c.contains("implement") || c.contains("not supported")),
        "disable must honestly name the missing migration engine; got={c:?}"
    );
}

#[test]
fn encryption_unknown_subcommand_fails() {
    let r = run(&["encryption", "frobnicate"]);
    assert_ne!(r.code, 0, "unknown encryption subcommand must fail");
    assert!(
        !r.combined_lower().contains("panicked"),
        "must not panic; stderr={:?}",
        r.stderr
    );
}

#[test]
fn encryption_no_subcommand_shows_usage() {
    let r = run(&["encryption"]);
    assert_ne!(
        r.code, 0,
        "encryption with no subcommand must fail with usage"
    );
    assert!(
        r.combined_lower().contains("usage"),
        "must mention usage; stderr={:?} stdout={:?}",
        r.stderr,
        r.stdout
    );
}

#[test]
fn encryption_verify_disabled_is_informational() {
    // No configured DB -> nothing encrypted to verify -> informational exit 0.
    let r = run(&["encryption", "verify"]);
    assert_eq!(
        r.code, 0,
        "verify with no encryption is informational (exit 0); stderr={:?}",
        r.stderr
    );
    assert!(
        r.stdout.to_lowercase().contains("not enabled")
            || r.stdout.to_lowercase().contains("nothing to verify")
            || r.stdout.to_lowercase().contains("disabled"),
        "verify must say there is nothing to verify; stdout={:?}",
        r.stdout
    );
}

// ============================================================================
// Encrypted-database fixtures (require config-toml to author the ALETHEIADB_CONFIG
// the CLI child opens). Default builds include config-toml.
// ============================================================================

#[cfg(feature = "config-toml")]
mod encrypted {
    use super::*;
    use aletheiadb::AletheiaDB;
    use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
    use aletheiadb::encryption::FileKeyProvider;
    use aletheiadb::encryption::config::EncryptionConfig;
    use aletheiadb::storage::index_persistence::PersistenceConfig;
    use aletheiadb::storage::wal::DurabilityMode;
    use aletheiadb::{PropertyMap, PropertyMapBuilder};

    /// A prepared encrypted-database fixture: the on-disk data dir plus the
    /// TOML config path the CLI child opens via `ALETHEIADB_CONFIG`.
    struct EncryptedFixture {
        _dir: TempDir,
        toml_path: std::path::PathBuf,
        key_path: std::path::PathBuf,
    }

    /// Build the AletheiaDBConfig for an encrypted, persistent database rooted
    /// at `root` and keyed by `key`.
    fn encrypted_config(root: &Path, key: &Path) -> AletheiaDBConfig {
        AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(root.join("wal"))
                    .durability_mode(DurabilityMode::GroupCommit {
                        max_delay_ms: 5,
                        max_batch_size: 64,
                    })
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: root.join("data"),
                load_on_startup: true,
                ..Default::default()
            })
            .encryption(EncryptionConfig::file_based(key))
            .build()
    }

    /// Create an encrypted database on disk with persisted index files, then
    /// drop it, leaving a TOML config the CLI can reopen.
    fn make_encrypted_db() -> EncryptedFixture {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let key_path = root.join("master.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        let toml_path = root.join("aletheia.toml");
        let config = encrypted_config(root, &key_path);
        config.to_toml_file(&toml_path).unwrap();

        // Seed from the SAME TOML the CLI will open, so paths match exactly.
        {
            let cfg = AletheiaDBConfig::from_toml_file(&toml_path).unwrap();
            let db = AletheiaDB::with_unified_config(cfg).unwrap();
            let a = db
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )
                .unwrap();
            let b = db
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();
            db.create_edge(a, b, "KNOWS", PropertyMap::new()).unwrap();
            db.persist_indexes().unwrap();
        }

        EncryptedFixture {
            _dir: dir,
            toml_path,
            key_path,
        }
    }

    fn cfg_env(f: &EncryptedFixture) -> Vec<(&'static str, String)> {
        vec![("ALETHEIADB_CONFIG", f.toml_path.display().to_string())]
    }

    /// Adapt owned env pairs to the &str slice `run_env` wants.
    fn run_with(args: &[&str], env: &[(&'static str, String)]) -> CliRun {
        let borrowed: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        run_env(args, &borrowed)
    }

    #[test]
    fn keys_rotate_status_fresh_encrypted_reports_no_pending() {
        let f = make_encrypted_db();
        let r = run_with(&["keys", "rotate", "--status"], &cfg_env(&f));
        assert_eq!(
            r.code, 0,
            "rotate --status on a fresh encrypted DB must exit 0; stderr={:?}",
            r.stderr
        );
        let c = r.combined_lower();
        assert!(
            c.contains("no rotation") || c.contains("fully") || c.contains("current"),
            "status must report no pending rotation / fully at current key; got={c:?}"
        );
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn keys_rotate_start_uniform_encrypted_refuses_cross_layer() {
        let f = make_encrypted_db();
        // A brand-new key to rotate TO.
        let new_key = f.toml_path.parent().unwrap().join("rotate-to.key");
        FileKeyProvider::generate_key_file(&new_key).unwrap();

        let r = run_with(
            &["keys", "rotate", "--new-key", new_key.to_str().unwrap()],
            &cfg_env(&f),
        );
        assert_ne!(
            r.code, 0,
            "rotate on a uniformly-encrypted DB must refuse (WAL encrypted); stdout={:?}",
            r.stdout
        );
        let c = r.combined_lower();
        assert!(
            c.contains("wal") && c.contains("encrypted"),
            "refusal must name the conflicting WAL layer; got={c:?}"
        );
        assert!(!c.contains("panicked"), "must not panic; got={c:?}");
        assert_no_key_leak(&r, &[f.key_path.as_path(), new_key.as_path()]);
    }

    #[test]
    fn keys_rotate_cancel_nothing_pending_reports_clearly() {
        let f = make_encrypted_db();
        let r = run_with(&["keys", "rotate", "--cancel"], &cfg_env(&f));
        assert_ne!(
            r.code, 0,
            "cancel with nothing pending must exit non-zero; stdout={:?}",
            r.stdout
        );
        let c = r.combined_lower();
        assert!(
            c.contains("no key rotation")
                || c.contains("not in progress")
                || c.contains("no rotation"),
            "cancel must clearly say nothing is pending; got={c:?}"
        );
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn encryption_status_encrypted_shows_per_layer_table() {
        let f = make_encrypted_db();
        let r = run_with(&["encryption", "status"], &cfg_env(&f));
        assert_eq!(
            r.code, 0,
            "encryption status on an encrypted DB must exit 0; stderr={:?}",
            r.stderr
        );
        let c = r.combined_lower();
        assert!(c.contains("enabled"), "overall must be ENABLED; got={c:?}");
        assert!(
            c.contains("wal") && c.contains("index"),
            "per-layer table must name WAL and Index layers; got={c:?}"
        );
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn encryption_verify_good_encrypted_passes() {
        let f = make_encrypted_db();
        let r = run_with(&["encryption", "verify"], &cfg_env(&f));
        assert_eq!(
            r.code, 0,
            "verify of a good encrypted DB must pass (exit 0); stderr={:?}",
            r.stderr
        );
        let c = r.combined_lower();
        assert!(
            c.contains("pass") || c.contains("verified") || c.contains("ok"),
            "verify must report success; got={c:?}"
        );
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn encryption_verify_missing_key_fails_clearly() {
        let f = make_encrypted_db();
        // Author a second TOML whose key path does not exist -> the cipher
        // cannot be constructed -> open fails -> verify fails.
        let bad_key = f.toml_path.parent().unwrap().join("does-not-exist.key");
        let bad_config = encrypted_config(f.toml_path.parent().unwrap(), &bad_key);
        let bad_toml = f.toml_path.parent().unwrap().join("bad.toml");
        bad_config.to_toml_file(&bad_toml).unwrap();

        let r = run_with(
            &["encryption", "verify"],
            &[("ALETHEIADB_CONFIG", bad_toml.display().to_string())],
        );
        assert_ne!(
            r.code, 0,
            "verify with a missing key must fail; stdout={:?}",
            r.stdout
        );
        let c = r.combined_lower();
        assert!(
            !c.contains("panicked"),
            "must fail cleanly, not panic; got={c:?}"
        );
        // Nothing decryptable happened; the real key's hex cannot appear.
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }
}
