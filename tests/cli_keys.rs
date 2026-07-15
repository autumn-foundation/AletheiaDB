//! End-to-end tests for the `aletheia keys` subcommand group (Issue #490).
//!
//! Covers the independent, non-rotation slice of the encryption operator CLI:
//! `keys generate`, `keys status` (alias `info`), and `keys verify`. Each test
//! invokes the real built binary via `env!("CARGO_BIN_EXE_aletheia")` in its own
//! process, mirroring the harness in `tests/cli_aletheia.rs`.
//!
//! Security-critical assertions: generated/verified output must NEVER contain
//! the raw key hex, and failures must not leak key material.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// Result of running the CLI binary once.
struct CliRun {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the `aletheia` binary with `args`, with `ALETHEIADB_CONFIG` and
/// `ALETHEIADB_DATA_DIR` explicitly removed so the child sees no ambient
/// database configuration (keeps `keys status` deterministic).
fn run(args: &[&str]) -> CliRun {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aletheia"));
    cmd.args(args);
    cmd.env_remove("ALETHEIADB_CONFIG");
    cmd.env_remove("ALETHEIADB_DATA_DIR");
    let output = cmd.output().expect("failed to spawn aletheia binary");
    CliRun {
        code: output.status.code().expect("process terminated by signal"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Read a generated key file's raw contents (the 64-char hex + newline).
fn read_key(path: &Path) -> String {
    std::fs::read_to_string(path).expect("key file should be readable")
}

/// The bare 64-char hex string of a key file (trimmed of the trailing newline).
fn key_hex(path: &Path) -> String {
    read_key(path).trim().to_string()
}

// ============================================================================
// keys generate
// ============================================================================

#[test]
fn keys_generate_creates_key_file() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("master.key");

    let r = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(
        r.code, 0,
        "keys generate must exit 0; stderr={:?}",
        r.stderr
    );
    assert!(key_path.exists(), "key file must exist after generation");

    // 64 hex chars + trailing newline.
    let raw = read_key(&key_path);
    assert_eq!(
        raw.trim().len(),
        64,
        "key must be 64 hex chars; got {raw:?}"
    );
    assert!(
        raw.trim().chars().all(|c| c.is_ascii_hexdigit()),
        "key must be hex; got {raw:?}"
    );
}

#[cfg(unix)]
#[test]
fn keys_generate_sets_0600_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("perm.key");

    let r = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(
        r.code, 0,
        "keys generate must exit 0; stderr={:?}",
        r.stderr
    );

    let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "key file must be 0600, got {mode:o}");
}

#[test]
fn keys_generate_refuses_overwrite_without_force() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("existing.key");

    let first = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(first.code, 0, "first generate must succeed");
    let original = read_key(&key_path);

    let second = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_ne!(second.code, 0, "second generate without --force must fail");
    assert_eq!(
        read_key(&key_path),
        original,
        "key file must be unchanged when overwrite is refused"
    );
}

#[test]
fn keys_generate_force_overwrites() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("force.key");

    let first = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(first.code, 0, "first generate must succeed");
    let original = read_key(&key_path);

    let second = run(&[
        "keys",
        "generate",
        "--output",
        key_path.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(
        second.code, 0,
        "generate --force must succeed; stderr={:?}",
        second.stderr
    );
    assert_ne!(
        read_key(&key_path),
        original,
        "--force must write a fresh key (overwhelmingly likely to differ)"
    );
}

#[test]
fn keys_generate_output_never_leaks_key_bytes() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("secret.key");

    let r = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(
        r.code, 0,
        "keys generate must exit 0; stderr={:?}",
        r.stderr
    );

    let hex = key_hex(&key_path);
    assert!(
        !r.stdout.contains(&hex) && !r.stderr.contains(&hex),
        "generate output must NEVER contain the raw key hex"
    );
}

#[test]
fn keys_generate_requires_output() {
    let r = run(&["keys", "generate"]);
    assert_ne!(r.code, 0, "keys generate with no --output must fail");
}

// ============================================================================
// keys status / info
// ============================================================================

#[test]
fn keys_status_not_configured_is_informational() {
    let r = run(&["keys", "status"]);
    assert_eq!(
        r.code, 0,
        "keys status with no config must be informational (exit 0); stderr={:?}",
        r.stderr
    );
    let lower = r.stdout.to_lowercase();
    assert!(
        lower.contains("disabled") || lower.contains("not configured"),
        "status must clearly state encryption is not configured; stdout={:?}",
        r.stdout
    );
}

#[test]
fn keys_status_with_key_file_shows_provider_version_algorithm() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("status.key");
    let g = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(g.code, 0, "generate must succeed");

    let r = run(&["keys", "status", "--key-file", key_path.to_str().unwrap()]);
    assert_eq!(r.code, 0, "keys status must exit 0; stderr={:?}", r.stderr);

    let out = r.stdout.to_lowercase();
    assert!(
        out.contains("file"),
        "status must name the provider type; stdout={:?}",
        r.stdout
    );
    assert!(
        out.contains("version"),
        "status must show key version; stdout={:?}",
        r.stdout
    );
    // Algorithm line (Auto/AES/ChaCha) must be present.
    assert!(
        out.contains("algorithm"),
        "status must show algorithm; stdout={:?}",
        r.stdout
    );

    // Path is not secret, but key bytes must never appear.
    let hex = key_hex(&key_path);
    assert!(
        !r.stdout.contains(&hex),
        "status must NEVER contain the raw key hex"
    );
}

#[test]
fn keys_info_is_an_alias_for_status() {
    let r = run(&["keys", "info"]);
    assert_eq!(r.code, 0, "keys info must exit 0; stderr={:?}", r.stderr);
    let lower = r.stdout.to_lowercase();
    assert!(
        lower.contains("disabled") || lower.contains("not configured"),
        "info alias must behave like status; stdout={:?}",
        r.stdout
    );
}

// ============================================================================
// keys verify
// ============================================================================

#[test]
fn keys_verify_valid_key_succeeds() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("valid.key");
    let g = run(&["keys", "generate", "--output", key_path.to_str().unwrap()]);
    assert_eq!(g.code, 0, "generate must succeed");

    let r = run(&["keys", "verify", "--key-file", key_path.to_str().unwrap()]);
    assert_eq!(
        r.code, 0,
        "verify of a valid key must exit 0; stderr={:?}",
        r.stderr
    );
    assert!(
        r.stdout.to_lowercase().contains("valid") || r.stdout.to_lowercase().contains("ok"),
        "verify must report success; stdout={:?}",
        r.stdout
    );

    // Success output must not leak the key.
    let hex = key_hex(&key_path);
    assert!(
        !r.stdout.contains(&hex),
        "verify success output must NEVER contain the raw key hex"
    );
}

#[test]
fn keys_verify_missing_key_fails_safely() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nope.key");

    let r = run(&["keys", "verify", "--key-file", missing.to_str().unwrap()]);
    assert_ne!(r.code, 0, "verify of a missing key must fail");
    // No key material can leak from a file that does not exist; assert the
    // error is a clean category message, not a panic.
    assert!(
        !r.stderr.contains("panicked"),
        "verify must fail cleanly, not panic; stderr={:?}",
        r.stderr
    );
}

#[test]
fn keys_verify_garbage_key_fails_without_leak() {
    let dir = TempDir::new().unwrap();
    let key_path = dir.path().join("garbage.key");
    // 17 bytes: neither 64 hex chars nor 32 raw bytes -> invalid format.
    let garbage = [0xABu8; 17];
    std::fs::write(&key_path, garbage).unwrap();

    let r = run(&["keys", "verify", "--key-file", key_path.to_str().unwrap()]);
    assert_ne!(r.code, 0, "verify of a malformed key must fail");
    assert!(
        !r.stderr.contains("panicked"),
        "verify must fail cleanly, not panic; stderr={:?}",
        r.stderr
    );

    // No-leak contract: the file's raw bytes must never appear in output, in
    // any encoding a future error message might use (a hex dump or the literal
    // bytes), so the guarantee is airtight against error-message changes.
    let hex: String = garbage.iter().map(|b| format!("{b:02x}")).collect();
    let byte_string = String::from_utf8_lossy(&garbage).into_owned();
    for out in [&r.stdout, &r.stderr] {
        assert!(
            !out.contains(&hex),
            "verify output must NEVER contain the file's hex bytes; got={out:?}"
        );
        assert!(
            !out.contains(&byte_string),
            "verify output must NEVER contain the file's raw bytes; got={out:?}"
        );
    }
}

#[test]
fn keys_verify_requires_key_file() {
    let r = run(&["keys", "verify"]);
    assert_ne!(r.code, 0, "verify with no --key-file must fail");
}

// ============================================================================
// keys routing
// ============================================================================

#[test]
fn keys_unknown_subcommand_fails() {
    let r = run(&["keys", "frobnicate"]);
    assert_ne!(r.code, 0, "unknown keys subcommand must fail");
}

#[test]
fn keys_rotate_bare_shows_usage_not_deferred_or_unknown() {
    // `keys rotate` is now wired to the shipped rotation engine (Issue #488).
    // Bare (no key source / action flag) it is a USAGE error -- no longer the
    // old "not yet available" deferred stub, and not the generic
    // "unknown keys subcommand" fallback. (Full behavior lives in
    // tests/cli_encryption.rs.)
    let r = run(&["keys", "rotate"]);
    assert_ne!(
        r.code, 0,
        "bare keys rotate must exit non-zero (usage error)"
    );
    let combined = format!("{}{}", r.stdout, r.stderr).to_lowercase();
    assert!(
        combined.contains("rotate"),
        "keys rotate usage must name the verb; stdout={:?} stderr={:?}",
        r.stdout,
        r.stderr
    );
    assert!(
        !combined.contains("not yet available"),
        "keys rotate must no longer report the deferred stub; got={combined:?}"
    );
    assert!(
        !combined.contains("unknown keys subcommand"),
        "keys rotate must not use the generic unknown-subcommand message; got={combined:?}"
    );
}

#[test]
fn keys_no_subcommand_shows_usage() {
    let r = run(&["keys"]);
    assert_ne!(r.code, 0, "keys with no subcommand must fail with usage");
    assert!(
        r.stderr.to_lowercase().contains("usage") || r.stdout.to_lowercase().contains("usage"),
        "keys with no subcommand must mention usage; stderr={:?}",
        r.stderr
    );
}
