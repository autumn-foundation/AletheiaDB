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
//! ## Architectural note (rotation scope: WAL + index succeed, cold still refuses)
//!
//! The rotation engine (`AletheiaDB::rotate_index_keys`) re-keys the index and,
//! since Issue #3617 PR2, the WAL as well (checkpoint -> force-roll under the new
//! WAL DEK -> truncate the old segments), so the WAL is no longer a cross-layer
//! conflict. AletheiaDB's config encrypts *uniformly* (enabling encryption
//! encrypts the WAL too), and the standard `make_encrypted_db` fixture configures
//! only WAL + index persistence with NO cold storage. Therefore
//! `keys rotate --new-key` / `--new-env-var` against that fixture now SUCCEEDS
//! (exit 0), re-encrypting every persisted file and bumping the key version --
//! and that success is what these tests assert.
//!
//! The one at-rest layer NOT yet covered is cold storage (PR3): when a tiered
//! cold store is configured under the same master key, an index+WAL rotation
//! would strand the cold values, so the cross-layer guard STILL refuses and
//! names `cold_storage`. `keys_rotate_start_with_cold_storage_refuses_cross_layer`
//! asserts that CLI refusal end-to-end (the unit test
//! `rotate_still_refuses_when_cold_storage_encrypted` covers the engine level).

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

/// Decode a hex string into bytes, or `None` if it is not valid even-length hex.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// Standard-alphabet base64 (with `=` padding) of `data` — a self-contained
/// encoder so the no-leak check does not depend on the optional `base64` crate
/// feature being enabled in the test build.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Assert no encoding of any key in `key_files` leaks into the output: the raw
/// key hex, its base64, AND its raw 32-byte form. Catching all three makes the
/// no-leak guarantee airtight against a future error message that reaches for a
/// different encoding of the same secret.
fn assert_no_key_leak(r: &CliRun, key_files: &[&Path]) {
    for kf in key_files {
        let raw = std::fs::read_to_string(kf).unwrap_or_default();
        let hex = raw.trim().to_string();
        if hex.is_empty() {
            continue;
        }
        let mut forbidden = vec![hex.clone()];
        if let Some(bytes) = hex_decode(&hex) {
            forbidden.push(base64_encode(&bytes));
            // The raw 32-byte form, as it would appear in lossily-decoded output.
            forbidden.push(String::from_utf8_lossy(&bytes).into_owned());
        }
        for out in [&r.stdout, &r.stderr] {
            for secret in &forbidden {
                if secret.is_empty() {
                    continue;
                }
                assert!(
                    !out.contains(secret.as_str()),
                    "output must NEVER contain any encoding of the key from {kf:?}; \
                     leaked={secret:?} got={out:?}"
                );
            }
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

#[test]
fn keys_rotate_two_actions_reports_choose_exactly_one() {
    // --new-key together with --status selects two actions; the guard rejects it
    // BEFORE opening any database, so no config is needed.
    let dir = TempDir::new().unwrap();
    let key = dir.path().join("new.key");
    let g = run(&["keys", "generate", "--output", key.to_str().unwrap()]);
    assert_eq!(g.code, 0, "generate must succeed");

    let r = run(&[
        "keys",
        "rotate",
        "--new-key",
        key.to_str().unwrap(),
        "--status",
    ]);
    assert_ne!(r.code, 0, "two actions must be a usage error");
    let c = r.combined_lower();
    assert!(
        c.contains("exactly one"),
        "must tell the operator to choose exactly one action; got={c:?}"
    );
    assert!(!c.contains("panicked"), "must not panic; got={c:?}");
    assert_no_key_leak(&r, &[key.as_path()]);
}

#[test]
fn keys_rotate_new_key_missing_value_is_usage_error() {
    // `--new-key` with no following value selects no action (arg_value yields
    // None), so it degrades to the bare usage error rather than panicking.
    let r = run(&["keys", "rotate", "--new-key"]);
    assert_ne!(r.code, 0, "missing --new-key value must fail");
    let c = r.combined_lower();
    assert!(
        c.contains("usage") || c.contains("rotate"),
        "must show rotate usage; got={c:?}"
    );
    assert!(!c.contains("panicked"), "must not panic; got={c:?}");
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
    use aletheiadb::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};
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

    /// Create an encrypted database WITH an encrypted cold tier configured (in
    /// addition to WAL + index persistence), then drop it, leaving a TOML config
    /// the CLI can reopen. Used to assert that key rotation still refuses while a
    /// cold tier is present (Issue #3617 PR3 is the cold re-keyer follow-up).
    fn make_encrypted_db_with_cold() -> EncryptedFixture {
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let key_path = root.join("master.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        let config = AletheiaDBConfig::builder()
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
            .historical(
                HistoricalConfigBuilder::new()
                    .enable_cold_storage(true)
                    .cold_storage_path(root.join("cold.redb"))
                    .migration_age_threshold(Duration::from_secs(3600))
                    .build(),
            )
            .encryption(EncryptionConfig::file_based(&key_path))
            .build();

        let toml_path = root.join("aletheia.toml");
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
    fn keys_rotate_start_uniform_encrypted_succeeds() {
        // Issue #3617 PR2: a uniformly-encrypted DB with WAL + index persistence
        // and NO cold storage is now fully rotatable. The WAL is re-keyed by the
        // checkpoint -> force-roll -> truncate driver rather than treated as a
        // cross-layer conflict, so `keys rotate --new-key` SUCCEEDS: it
        // re-encrypts every persisted index file and bumps the key version.
        let f = make_encrypted_db();
        // A brand-new key to rotate TO.
        let new_key = f.toml_path.parent().unwrap().join("rotate-to.key");
        FileKeyProvider::generate_key_file(&new_key).unwrap();

        let r = run_with(
            &["keys", "rotate", "--new-key", new_key.to_str().unwrap()],
            &cfg_env(&f),
        );
        assert_eq!(
            r.code, 0,
            "rotate on a uniform WAL+index-encrypted DB (no cold) must succeed; \
             stdout={:?} stderr={:?}",
            r.stdout, r.stderr
        );
        let c = r.combined_lower();
        // Success summary: completion headline + old->new key-version bump + the
        // re-encrypted file count (the CLI's print_rotation_report / progress line).
        assert!(
            c.contains("rotation complete"),
            "success must print the completion headline; got={c:?}"
        );
        assert!(
            c.contains("key version") && c.contains("->"),
            "success must print the old->new key-version bump; got={c:?}"
        );
        assert!(
            c.contains("re-encrypted"),
            "success must report the re-encrypted file count; got={c:?}"
        );
        // It must NOT be the old cross-layer refusal.
        assert!(
            !c.contains("other encrypted-at-rest layers"),
            "must not be the cross-layer refusal; got={c:?}"
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
        // The WAL and Index rows must actually report ENCRYPTED, not merely
        // appear as layer names.
        let out_lower = r.stdout.to_lowercase();
        for row in ["wal:", "index:"] {
            let line = out_lower
                .lines()
                .find(|l| l.trim_start().starts_with(row))
                .unwrap_or_else(|| panic!("per-layer table must have a {row:?} row; got={c:?}"));
            assert!(
                line.contains("encrypted"),
                "the {row:?} row must report ENCRYPTED; got line={line:?}"
            );
        }
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn keys_rotate_resume_nothing_pending_reports_clearly() {
        // A fresh encrypted DB has no rotation.state breadcrumb, so `--resume`
        // takes the Ok(None) path: exit 0 with a clear "nothing to resume"
        // message (drives AletheiaDB::resume_pending_index_rotation, Issue #490).
        let f = make_encrypted_db();
        let r = run_with(&["keys", "rotate", "--resume"], &cfg_env(&f));
        assert_eq!(
            r.code, 0,
            "resume with nothing pending must exit 0; stderr={:?}",
            r.stderr
        );
        let c = r.combined_lower();
        assert!(
            c.contains("no pending") && c.contains("resume"),
            "resume must clearly say nothing is pending; got={c:?}"
        );
        assert!(!c.contains("panicked"), "must not panic; got={c:?}");
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn keys_rotate_start_via_env_var_succeeds() {
        // Exercises the `--new-env-var` start path (KeyProviderConfig::Env
        // branch). Issue #3617 PR2: a uniform WAL+index DB (no cold) is now
        // rotatable, so the guard no longer short-circuits -- the env var IS
        // sourced and must hold the hex-encoded new MEK. We generate a fresh key
        // and export its hex, then assert the rotation succeeds.
        let f = make_encrypted_db();
        let new_key = f.toml_path.parent().unwrap().join("rotate-to.key");
        FileKeyProvider::generate_key_file(&new_key).unwrap();
        // EnvKeyProvider reads a hex-encoded MEK from the named variable.
        let new_key_hex = std::fs::read_to_string(&new_key)
            .unwrap()
            .trim()
            .to_string();

        let mut env = cfg_env(&f);
        env.push(("ALETHEIADB_MEK_NEW", new_key_hex));
        let r = run_with(
            &["keys", "rotate", "--new-env-var", "ALETHEIADB_MEK_NEW"],
            &env,
        );
        assert_eq!(
            r.code, 0,
            "rotate via env-var on a uniform WAL+index-encrypted DB (no cold) must \
             succeed; stdout={:?} stderr={:?}",
            r.stdout, r.stderr
        );
        let c = r.combined_lower();
        assert!(
            c.contains("rotation complete"),
            "success must print the completion headline; got={c:?}"
        );
        assert!(
            c.contains("key version") && c.contains("->"),
            "success must print the old->new key-version bump; got={c:?}"
        );
        assert!(
            c.contains("re-encrypted"),
            "success must report the re-encrypted file count; got={c:?}"
        );
        assert!(
            !c.contains("other encrypted-at-rest layers"),
            "must not be the cross-layer refusal; got={c:?}"
        );
        assert!(!c.contains("panicked"), "must not panic; got={c:?}");
        // The exported hex is a key encoding: it must never leak into output.
        assert_no_key_leak(&r, &[f.key_path.as_path(), new_key.as_path()]);
    }

    #[test]
    fn keys_rotate_start_with_cold_storage_refuses_cross_layer() {
        // Issue #3617 PR2: cold storage is NOT yet covered (PR3). A DB with an
        // encrypted cold tier configured must STILL refuse an index+WAL rotation
        // (it would strand the cold values under the old MEK). This is the CLI
        // end-to-end counterpart to the engine unit test
        // `rotate_still_refuses_when_cold_storage_encrypted`.
        let f = make_encrypted_db_with_cold();
        let new_key = f.toml_path.parent().unwrap().join("rotate-to.key");
        FileKeyProvider::generate_key_file(&new_key).unwrap();

        let r = run_with(
            &["keys", "rotate", "--new-key", new_key.to_str().unwrap()],
            &cfg_env(&f),
        );
        assert_ne!(
            r.code, 0,
            "rotate with encrypted cold storage configured must still refuse; stdout={:?}",
            r.stdout
        );
        let c = r.combined_lower();
        assert!(
            c.contains("cold_storage"),
            "refusal must name the conflicting cold_storage layer; got={c:?}"
        );
        assert!(!c.contains("panicked"), "must not panic; got={c:?}");
        assert_no_key_leak(&r, &[f.key_path.as_path(), new_key.as_path()]);
    }

    /// Overwrite the 4-byte little-endian `key_version` field (offset 6..10) of
    /// the first `AEIX`-headed index file found under `indexes_dir` with
    /// `version`, leaving the rest of the file intact. Returns the tampered
    /// file's path. Mirrors the on-disk header layout in
    /// `storage::index_persistence::common` (`[AEIX:4][fmt:1][alg:1][ver:4]`).
    fn stamp_first_index_file_version(indexes_dir: &Path, version: u32) -> std::path::PathBuf {
        fn walk(dir: &Path, version: u32) -> Option<std::path::PathBuf> {
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(hit) = walk(&p, version) {
                        return Some(hit);
                    }
                    continue;
                }
                let mut bytes = match std::fs::read(&p) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                if bytes.len() >= 10 && &bytes[..4] == b"AEIX" {
                    bytes[6..10].copy_from_slice(&version.to_le_bytes());
                    std::fs::write(&p, &bytes).unwrap();
                    return Some(p);
                }
            }
            None
        }
        walk(indexes_dir, version).expect("no AEIX-headed index file found to tamper")
    }

    /// Corrupt the encrypted BODY of the named `AEIX`-headed index file under
    /// `indexes_dir` while leaving its 10-byte plaintext header (magic, format,
    /// algorithm, `key_version`) byte-for-byte intact: flip the final byte (the
    /// AES-GCM tag), guaranteeing the AEAD auth check fails on decrypt even
    /// though header classification still passes. Returns the tampered path.
    fn corrupt_index_body_named(indexes_dir: &Path, file_name: &str) -> std::path::PathBuf {
        fn walk(dir: &Path, file_name: &str) -> Option<std::path::PathBuf> {
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(hit) = walk(&p, file_name) {
                        return Some(hit);
                    }
                    continue;
                }
                if p.file_name().and_then(|n| n.to_str()) != Some(file_name) {
                    continue;
                }
                let mut bytes = match std::fs::read(&p) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                // Header intact (>=10 bytes, AEIX magic); flip the last body byte.
                if bytes.len() > 10 && &bytes[..4] == b"AEIX" {
                    let last = bytes.len() - 1;
                    bytes[last] ^= 0xFF;
                    std::fs::write(&p, &bytes).unwrap();
                    return Some(p);
                }
            }
            None
        }
        walk(indexes_dir, file_name).expect("named AEIX-headed index file not found to corrupt")
    }

    /// Author a TOML config for the fixture's data dir/key but with index
    /// persistence `load_on_startup = false`, so a corrupted index BODY does not
    /// fail the `open()` (the index files are not force-loaded) — the exact
    /// window in which header-only classification would false-PASS (Issue #3618).
    fn no_load_toml(f: &EncryptedFixture) -> std::path::PathBuf {
        let root = f.toml_path.parent().unwrap();
        let config = AletheiaDBConfig::builder()
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
                load_on_startup: false,
                ..Default::default()
            })
            .encryption(EncryptionConfig::file_based(&f.key_path))
            .build();
        let toml_path = root.join("no-load.toml");
        config.to_toml_file(&toml_path).unwrap();
        toml_path
    }

    #[test]
    fn encryption_verify_same_version_undecryptable_body_fails() {
        // Issue #3618: the false-PASS the old header-only classifier could not
        // catch. Corrupt one index file's BODY (AES-GCM tag) while leaving its
        // AEIX header/key_version intact, and open with load_on_startup=false so
        // the corrupted body is NOT force-loaded at open(). Under the OLD code
        // path, header classification finds version 1 held by the keyring and
        // `encryption verify` would PASS. The #3618 decrypt probe attempts a real
        // body decrypt, the AEAD auth tag fails, and verify now FAILS instead.
        //
        // Why corruption, not a fully-wrong configured KEY, is the constructible
        // end-to-end probe trigger: WAL and index share one MEK, so a wrong
        // configured key aborts at open_db() during WAL replay BEFORE the index
        // probe ever runs — it cannot be reached via the CLI. Corrupting one index
        // body (header intact, load_on_startup=false so it is not force-loaded at
        // open) is the only way to drive the probe to a FAIL end-to-end. The
        // body-level wrong-key path is covered by the multi-generation unit test
        // (verify_decryptable_catches_wrong_key_in_one_of_multiple_generations).
        let f = make_encrypted_db();
        let indexes_dir = f.toml_path.parent().unwrap().join("data").join("indexes");
        // Corrupt the manifest body (always probed by verify_decryptable).
        let tampered = corrupt_index_body_named(&indexes_dir, "manifest.idx");
        assert!(tampered.exists(), "the manifest should have been corrupted");

        let toml = no_load_toml(&f);
        let r = run_with(
            &["encryption", "verify"],
            &[("ALETHEIADB_CONFIG", toml.display().to_string())],
        );
        assert_ne!(
            r.code, 0,
            "verify with an undecryptable index body must FAIL; stdout={:?} stderr={:?}",
            r.stdout, r.stderr
        );
        let c = r.combined_lower();
        assert!(c.contains("failed"), "verify must report FAILED; got={c:?}");
        assert!(
            !c.contains("verify: pass"),
            "verify must NOT false-PASS on an undecryptable body; got={c:?}"
        );
        // The probe's FAIL wording: wrong key / corruption, does not decrypt.
        assert!(
            c.contains("does not decrypt") || c.contains("wrong key") || c.contains("corruption"),
            "failure should name the undecryptable body; got={c:?}"
        );
        assert!(!c.contains("panicked"), "must not panic; got={c:?}");
        // No key bytes leak on stdout or stderr.
        assert_no_key_leak(&r, &[f.key_path.as_path()]);
    }

    #[test]
    fn encryption_verify_foreign_key_version_index_file_fails() {
        // Re-stamp one persisted index file's AEIX header with a key version the
        // configured keyring does NOT hold (as a file written under a foreign
        // key generation would carry). verify's index probe must classify it as
        // `unknown` and FAIL — not false-PASS. This is the genuine index
        // failure branch (Issue #490 review): a wrong-key with the SAME version
        // number cannot be caught by header classification alone, but a file the
        // keyring cannot account for is, and a real scan/IO error propagates as
        // FAILED rather than being swallowed as a benign not-configured PASS.
        let f = make_encrypted_db();
        let indexes_dir = f.toml_path.parent().unwrap().join("data").join("indexes");
        let tampered = stamp_first_index_file_version(&indexes_dir, 0xFFFF_FFFF);
        assert!(tampered.exists(), "a file should have been tampered");

        let r = run_with(&["encryption", "verify"], &cfg_env(&f));
        assert_ne!(
            r.code, 0,
            "verify with a foreign-key-version index file must FAIL; stdout={:?}",
            r.stdout
        );
        let c = r.combined_lower();
        assert!(c.contains("failed"), "verify must report FAILED; got={c:?}");
        assert!(
            !c.contains("verify: pass"),
            "verify must NOT false-PASS on an unaccountable index file; got={c:?}"
        );
        assert!(!c.contains("panicked"), "must not panic; got={c:?}");
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
