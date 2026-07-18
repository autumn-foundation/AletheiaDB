//! Durable encryption-state authority — `{data_dir}/encryption.state` (Issue #3616).
//!
//! A tiny, versioned, line-based file that records the **authoritative**
//! encryption-at-rest posture of a durable database: whether encryption is on,
//! and — as a non-secret *reference*, never key bytes — where the Master
//! Encryption Key is sourced from. It is the durable authority the
//! plaintext↔encrypted migration engine (Issue #3616) flips so the *next*
//! [`open()`](crate::AletheiaDB::open) uses the correct mode, independent of the
//! operator's TOML/env config.
//!
//! # Why a separate file (not the TOML)?
//!
//! The operator's `ALETHEIADB_CONFIG` TOML (or `ALETHEIADB_DATA_DIR`, which is
//! hard-wired plaintext) is *input*, not durable database state. After an
//! `encryption enable` migration rewrites every at-rest byte through the cipher,
//! something inside the data dir must tell the next `open()` "this database is
//! now encrypted, under this key source" — otherwise the operator's unchanged
//! plaintext TOML would try to open encrypted bytes as plaintext and fail. This
//! file is that durable record.
//!
//! # Precedence (the authority WINS)
//!
//! [`read_encryption_state`] is consulted by `open()` (see
//! [`with_unified_config`](crate::AletheiaDB::with_unified_config)) as the
//! **authority** for the encryption on/off decision — it overrides the
//! TOML/env-derived [`EncryptionConfig`]. On divergence (the authority says one
//! thing, the config another) a LOUD warning naming BOTH sources is logged so an
//! operator is never silently overridden.
//!
//! # Fail-closed
//!
//! This file *guards encryption*, so it never guesses:
//! * **absent** → `Ok(None)` — not enabled via the authority; fall through to
//!   TOML/env (today's behavior, fully backward compatible).
//! * **present & well-formed** → `Ok(Some(_))`.
//! * **present but corrupt** → `Err(_)` — startup aborts loudly for manual
//!   intervention; a garbled authority is NEVER treated as "not enabled".
//!
//! When the authority says `enabled` but the referenced key source is
//! missing/unreadable, `open()` refuses (the encryption manager fails to source
//! the MEK) rather than falling back to plaintext.
//!
//! # Security
//!
//! Only a key-source **reference** is ever written — a File path or an Env var
//! NAME — exactly like the rotation ledger. Key bytes, passphrases, and tokens
//! are never persisted here; passphrase/KMS/Vault sources (which carry secrets
//! or multi-field config the line format cannot round-trip) are refused
//! fail-closed by [`write_encryption_state_durable`], mirroring the rotation
//! ledger's File/Env-only constraint.

use std::path::{Path, PathBuf};

use crate::core::error::{Result, StorageError};
use crate::encryption::config::KeyProviderConfig;
use crate::encryption::factory::Algorithm;

/// Current on-disk format version of the authority file.
///
/// `version=2` (Issue #3616 PR3) additively records the pinned concrete AEAD
/// `algorithm` an enable migration wrote under. The reader still accepts a legacy
/// `version=1` file (no `algorithm` line → `algorithm: None`, reopen falls back to
/// the config algorithm, exactly the prior behavior). An unknown version fails
/// closed (never guessed).
const ENCRYPTION_STATE_VERSION: u32 = 2;

/// Serialize a concrete AEAD [`Algorithm`] to its stable on-disk token. `Auto` is
/// resolved to its concrete form first so the pinned value is never ambiguous
/// (the whole point of pinning is cross-host portability).
fn algorithm_token(algorithm: Algorithm) -> &'static str {
    match algorithm.resolve() {
        Algorithm::Aes256Gcm => "aes256gcm",
        Algorithm::ChaCha20Poly1305 => "chacha20poly1305",
        // `resolve()` never returns `Auto`; be explicit rather than `unreachable!`.
        Algorithm::Auto => "aes256gcm",
    }
}

/// Parse a pinned-algorithm token written by [`algorithm_token`]. Returns `None`
/// for an unrecognized token (treated as corrupt by the caller).
fn algorithm_from_token(token: &str) -> Option<Algorithm> {
    match token {
        "aes256gcm" => Some(Algorithm::Aes256Gcm),
        "chacha20poly1305" => Some(Algorithm::ChaCha20Poly1305),
        _ => None,
    }
}

/// The durable encryption-state authority parsed from `{data_dir}/encryption.state`.
///
/// `enabled` is the authoritative on/off decision. `key_source` is present iff
/// `enabled` and is restricted to File/Env references (never key bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncryptionState {
    /// Whether encryption at rest is authoritatively ON for this database.
    pub enabled: bool,
    /// The non-secret key-source reference (File path / Env var name). `Some`
    /// iff `enabled`; `None` when the authority records "disabled".
    pub key_source: Option<KeyProviderConfig>,
    /// The pinned concrete AEAD algorithm the enable migration wrote under
    /// (Issue #3616 PR3). `Some` for a `version=2` enabled authority; `None` for a
    /// legacy `version=1` authority or a disabled state. On reopen it OVERRIDES the
    /// operator's `config.encryption.algorithm`, so an enable performed under a
    /// concrete (or host-resolved `Auto`) algorithm always reopens under the exact
    /// same cipher, regardless of TOML edits or a cross-CPU host migration.
    pub algorithm: Option<Algorithm>,
}

impl EncryptionState {
    /// Construct the "encryption enabled under `key_source`" authority WITHOUT a
    /// pinned algorithm (legacy shape). Prefer [`Self::enabled_with_algorithm`] on
    /// the production enable path so the concrete cipher is pinned durably.
    ///
    /// `key_source` must be a File or Env reference; other backends are refused
    /// at write time (they carry secrets the line format cannot persist).
    #[cfg(test)]
    pub(crate) fn enabled(key_source: KeyProviderConfig) -> Self {
        Self {
            enabled: true,
            key_source: Some(key_source),
            algorithm: None,
        }
    }

    /// Construct the "encryption enabled under `key_source`, pinned to
    /// `algorithm`" authority (Issue #3616 PR3). The algorithm is resolved to its
    /// concrete form so the pinned value is host-independent.
    ///
    /// The producer is the `encryption enable` migration engine
    /// (`crate::db::encryption_enable`), which flips this authority durably as the
    /// binding step BEFORE clearing the migration ledger.
    pub(crate) fn enabled_with_algorithm(
        key_source: KeyProviderConfig,
        algorithm: Algorithm,
    ) -> Self {
        Self {
            enabled: true,
            key_source: Some(key_source),
            algorithm: Some(algorithm.resolve()),
        }
    }
}

/// Path of the durable authority file within a data dir.
pub(crate) fn encryption_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("encryption.state")
}

/// Reduce a corrupt-file detail to a fail-closed error (never fabricates state).
fn corrupt(detail: &str) -> crate::core::error::Error {
    StorageError::InconsistentState {
        reason: format!(
            "encryption.state present but corrupt ({detail}); manual intervention required"
        ),
    }
    .into()
}

/// Read + parse the durable encryption-state authority for `data_dir`.
///
/// Fail-closed: absent → `Ok(None)`, well-formed → `Ok(Some(_))`, present but
/// corrupt / unreadable → `Err(_)`. A malformed authority is NEVER downgraded to
/// "not enabled" — that would silently open an encrypted database as plaintext.
///
/// Accepted key-source kinds are `file` / `env` only; any other kind (or an
/// `enabled=true` record with no key source) is corrupt.
pub(crate) fn read_encryption_state(data_dir: &Path) -> Result<Option<EncryptionState>> {
    read_encryption_state_at(&encryption_state_path(data_dir))
}

/// Path-based core of [`read_encryption_state`], so tests and future startup
/// wiring can target an exact file. Same fail-closed contract.
pub(crate) fn read_encryption_state_at(path: &Path) -> Result<Option<EncryptionState>> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StorageError::io_error(format!(
                "encryption.state present but unreadable ({e}); manual intervention required"
            ))
            .into());
        }
    };

    let mut version = None;
    let mut enabled = None;
    let mut kind = None;
    let mut value = None;
    let mut algorithm_line = None;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(corrupt("malformed line"));
        };
        match k {
            "version" => version = Some(v.parse::<u32>().map_err(|_| corrupt("bad version"))?),
            "enabled" => {
                enabled = Some(match v {
                    "true" => true,
                    "false" => false,
                    other => return Err(corrupt(&format!("bad enabled {other:?}"))),
                })
            }
            "key_source_kind" => kind = Some(v.to_string()),
            "key_source_value" => value = Some(v.to_string()),
            // Pinned concrete AEAD algorithm (Issue #3616 PR3, `version=2`).
            "algorithm" => algorithm_line = Some(v.to_string()),
            // Unknown keys are ignored for forward-compatibility, mirroring the
            // rotation ledger reader.
            _ => {}
        }
    }

    // `version=` is MANDATORY — a well-formed writer always emits it, so its
    // absence means a truncated/garbled file, not a legacy artifact.
    let version = version.ok_or_else(|| corrupt("missing version"))?;
    // Accept every version this build understands. A `version=1` file predates the
    // pinned `algorithm` line (Issue #3616 PR3) and loads with `algorithm: None` —
    // reopen then falls back to the config algorithm, exactly the prior behavior.
    if version != 1 && version != ENCRYPTION_STATE_VERSION {
        return Err(corrupt(&format!("unsupported version {version}")));
    }
    let enabled = enabled.ok_or_else(|| corrupt("missing enabled"))?;

    if !enabled {
        return Ok(Some(EncryptionState {
            enabled: false,
            key_source: None,
            algorithm: None,
        }));
    }

    // enabled → a File/Env key-source reference is required (fail-closed: an
    // enabled authority with no resolvable key source is corrupt, never a
    // silent plaintext fallback).
    let value = value.ok_or_else(|| corrupt("missing key_source_value"))?;
    let key_source = match kind.as_deref() {
        Some("file") => KeyProviderConfig::File { path: value.into() },
        Some("env") => KeyProviderConfig::Env { variable: value },
        Some(other) => return Err(corrupt(&format!("unsupported key_source_kind {other:?}"))),
        None => return Err(corrupt("missing key_source_kind")),
    };
    // A present `algorithm` line must parse to a known concrete algorithm; an
    // unrecognized token is corrupt (fail-closed, never a silent wrong cipher).
    // Absent (legacy `version=1`) → `None`.
    let algorithm = match algorithm_line {
        Some(token) => Some(
            algorithm_from_token(&token)
                .ok_or_else(|| corrupt(&format!("unsupported algorithm {token:?}")))?,
        ),
        None => None,
    };
    Ok(Some(EncryptionState {
        enabled: true,
        key_source: Some(key_source),
        algorithm,
    }))
}

/// Durably write the encryption-state authority for `data_dir`.
///
/// Uses the SAME ordered, crash-safe write as the rotation ledger — temp →
/// `sync_all` → atomic rename → parent-directory fsync
/// ([`write_durable`](crate::db::rotation::write_durable)) — so a power loss can
/// never leave a torn authority file. No key material is written: only a
/// non-secret File/Env reference.
///
/// # Errors
///
/// * The key source is a passphrase/KMS/Vault backend (refused fail-closed —
///   those carry secrets or multi-field config the line format cannot persist,
///   mirroring the rotation ledger).
/// * An enabled state carries no key source, or a disabled state carries one
///   (a caller bug — the authority must be internally consistent).
/// * The directory cannot be created or the file cannot be written durably.
///
/// The production caller is the `encryption enable` migration engine (Issue
/// #3616 PR3, `crate::db::encryption_enable`): `enable_encryption` flips the
/// authority to `enabled` as the binding step before clearing the ledger, and
/// `resume_pending_enable` performs the same flip when finishing an interrupted
/// enable.
pub(crate) fn write_encryption_state_durable(
    data_dir: &Path,
    state: &EncryptionState,
) -> Result<()> {
    let body = match (state.enabled, &state.key_source) {
        (true, Some(source)) => {
            let (kind, value) = match source {
                KeyProviderConfig::File { path } => ("file", path.to_string_lossy().into_owned()),
                KeyProviderConfig::Env { variable } => ("env", variable.clone()),
                KeyProviderConfig::PassphraseFile { .. }
                | KeyProviderConfig::Kms { .. }
                | KeyProviderConfig::Vault { .. } => {
                    let (provider_type, _) = source.describe();
                    return Err(StorageError::PersistenceError(format!(
                        "recording a {provider_type} key source in the encryption-state authority \
                         is not supported (only file/env references can be persisted without \
                         leaking a secret)"
                    ))
                    .into());
                }
            };
            // The pinned concrete algorithm line is additive (`version=2`); a state
            // constructed without one (legacy `enabled`) omits it and reads back as
            // `algorithm: None`.
            let algorithm_line = match state.algorithm {
                Some(algorithm) => format!("algorithm={}\n", algorithm_token(algorithm)),
                None => String::new(),
            };
            format!(
                "version={ENCRYPTION_STATE_VERSION}\nenabled=true\nkey_source_kind={kind}\nkey_source_value={value}\n{algorithm_line}"
            )
        }
        (true, None) => {
            return Err(StorageError::PersistenceError(
                "cannot record an enabled encryption-state authority with no key source"
                    .to_string(),
            )
            .into());
        }
        (false, Some(_)) => {
            return Err(StorageError::PersistenceError(
                "cannot record a disabled encryption-state authority with a key source".to_string(),
            )
            .into());
        }
        (false, None) => format!("version={ENCRYPTION_STATE_VERSION}\nenabled=false\n"),
    };

    std::fs::create_dir_all(data_dir)
        .map_err(|e| StorageError::io_error(format!("Failed to create data dir: {e}")))?;
    crate::db::rotation::write_durable(&encryption_state_path(data_dir), body.as_bytes())
}

/// Emit a LOUD warning under either logging configuration, matching the crate's
/// `observability`-gated logging style (mirrors `log_registry_warning` in
/// `src/db/snapshot.rs`).
pub(crate) fn log_authority_warning(message: &str) {
    #[cfg(feature = "observability")]
    tracing::warn!("{}", message);
    #[cfg(not(feature = "observability"))]
    eprintln!("WARNING: {}", message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn absent_reads_as_none() {
        let dir = tmp();
        assert_eq!(read_encryption_state(dir.path()).unwrap(), None);
    }

    #[test]
    fn round_trips_enabled_file_source() {
        let dir = tmp();
        let state = EncryptionState::enabled(KeyProviderConfig::File {
            path: "/etc/aletheia/mek.key".into(),
        });
        write_encryption_state_durable(dir.path(), &state).unwrap();
        assert_eq!(read_encryption_state(dir.path()).unwrap(), Some(state));
    }

    #[test]
    fn round_trips_enabled_env_source() {
        let dir = tmp();
        let state = EncryptionState::enabled(KeyProviderConfig::Env {
            variable: "ALETHEIADB_MEK".to_string(),
        });
        write_encryption_state_durable(dir.path(), &state).unwrap();
        assert_eq!(read_encryption_state(dir.path()).unwrap(), Some(state));
    }

    #[test]
    fn round_trips_disabled() {
        let dir = tmp();
        let state = EncryptionState {
            enabled: false,
            key_source: None,
            algorithm: None,
        };
        write_encryption_state_durable(dir.path(), &state).unwrap();
        assert_eq!(read_encryption_state(dir.path()).unwrap(), Some(state));
    }

    #[test]
    fn round_trips_pinned_algorithm() {
        // A version=2 authority pins the concrete algorithm; it round-trips.
        for algo in [Algorithm::Aes256Gcm, Algorithm::ChaCha20Poly1305] {
            let dir = tmp();
            let state = EncryptionState::enabled_with_algorithm(
                KeyProviderConfig::File {
                    path: "/etc/aletheia/mek.key".into(),
                },
                algo,
            );
            write_encryption_state_durable(dir.path(), &state).unwrap();
            let read = read_encryption_state(dir.path()).unwrap().unwrap();
            assert_eq!(read.algorithm, Some(algo));
            assert_eq!(read, state);
            // The pinned token is on disk under the additive `algorithm=` key.
            let text = std::fs::read_to_string(encryption_state_path(dir.path())).unwrap();
            assert!(text.contains("version=2"));
            assert!(text.contains("algorithm="));
        }
    }

    #[test]
    fn auto_is_pinned_to_a_concrete_algorithm() {
        // Pinning `Auto` records the host-resolved concrete algorithm, never `Auto`.
        let dir = tmp();
        let state = EncryptionState::enabled_with_algorithm(
            KeyProviderConfig::Env {
                variable: "MEK".to_string(),
            },
            Algorithm::Auto,
        );
        write_encryption_state_durable(dir.path(), &state).unwrap();
        let read = read_encryption_state(dir.path()).unwrap().unwrap();
        assert!(matches!(
            read.algorithm,
            Some(Algorithm::Aes256Gcm) | Some(Algorithm::ChaCha20Poly1305)
        ));
        assert_ne!(read.algorithm, Some(Algorithm::Auto));
    }

    #[test]
    fn legacy_v1_authority_reads_with_no_pinned_algorithm() {
        // A pre-#3616-PR3 `version=1` authority (no `algorithm` line) still loads,
        // with `algorithm: None` — reopen then falls back to the config algorithm.
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"version=1\nenabled=true\nkey_source_kind=env\nkey_source_value=MEK\n",
        )
        .unwrap();
        let read = read_encryption_state(dir.path()).unwrap().unwrap();
        assert!(read.enabled);
        assert_eq!(read.algorithm, None, "legacy v1 pins no algorithm");
    }

    #[test]
    fn corrupt_unknown_algorithm_token_fails_closed() {
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"version=2\nenabled=true\nkey_source_kind=env\nkey_source_value=MEK\nalgorithm=twofish\n",
        )
        .unwrap();
        assert!(read_encryption_state(dir.path()).is_err());
    }

    #[test]
    fn write_leaves_no_temp_file() {
        // The atomic write must not leave a `.tmp` sibling on success.
        let dir = tmp();
        write_encryption_state_durable(
            dir.path(),
            &EncryptionState::enabled(KeyProviderConfig::Env {
                variable: "MEK".to_string(),
            }),
        )
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }

    #[test]
    fn no_key_bytes_on_disk_only_reference() {
        // Hygiene: the file must carry only a path/name reference, never key
        // material. We seed a File source whose *path string* is distinctive and
        // assert the on-disk bytes contain the reference but nothing resembling
        // raw key bytes beyond it.
        let dir = tmp();
        write_encryption_state_durable(
            dir.path(),
            &EncryptionState::enabled(KeyProviderConfig::File {
                path: "/keys/unique-ref-path.key".into(),
            }),
        )
        .unwrap();
        let bytes = std::fs::read(encryption_state_path(dir.path())).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("/keys/unique-ref-path.key"));
        assert!(text.contains("key_source_kind=file"));
        // Only the four documented keys appear.
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let key = line.split_once('=').unwrap().0;
            assert!(
                matches!(
                    key,
                    "version" | "enabled" | "key_source_kind" | "key_source_value" | "algorithm"
                ),
                "unexpected key on disk: {key}"
            );
        }
    }

    #[test]
    fn corrupt_missing_version_fails_closed() {
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"enabled=true\nkey_source_kind=env\nkey_source_value=MEK\n",
        )
        .unwrap();
        assert!(read_encryption_state(dir.path()).is_err());
    }

    #[test]
    fn corrupt_unknown_version_fails_closed() {
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"version=99\nenabled=true\nkey_source_kind=env\nkey_source_value=MEK\n",
        )
        .unwrap();
        assert!(read_encryption_state(dir.path()).is_err());
    }

    #[test]
    fn corrupt_malformed_line_fails_closed() {
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"version=1\nthis_is_not_kv\n",
        )
        .unwrap();
        assert!(read_encryption_state(dir.path()).is_err());
    }

    #[test]
    fn enabled_without_key_source_is_corrupt() {
        // Fail-closed: an enabled authority MUST resolve a key source; a record
        // that says enabled but names none is corrupt, never a plaintext fallback.
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"version=1\nenabled=true\n",
        )
        .unwrap();
        assert!(read_encryption_state(dir.path()).is_err());
    }

    #[test]
    fn enabled_with_unknown_kind_is_corrupt() {
        let dir = tmp();
        std::fs::write(
            encryption_state_path(dir.path()),
            b"version=1\nenabled=true\nkey_source_kind=kms\nkey_source_value=arn:whatever\n",
        )
        .unwrap();
        assert!(read_encryption_state(dir.path()).is_err());
    }

    #[test]
    fn writing_secret_backed_source_is_refused() {
        // Passphrase/KMS/Vault sources are refused fail-closed — they carry
        // secrets or multi-field config the line format cannot round-trip.
        let dir = tmp();
        let state = EncryptionState {
            enabled: true,
            key_source: Some(KeyProviderConfig::PassphraseFile {
                path: "/k.aekf".into(),
                passphrase_env: "PASS".to_string(),
            }),
            algorithm: None,
        };
        assert!(write_encryption_state_durable(dir.path(), &state).is_err());
        // And nothing partial was left on disk.
        assert_eq!(read_encryption_state(dir.path()).unwrap(), None);
    }

    #[test]
    fn overwrite_replaces_prior_state() {
        let dir = tmp();
        write_encryption_state_durable(
            dir.path(),
            &EncryptionState::enabled(KeyProviderConfig::Env {
                variable: "OLD".to_string(),
            }),
        )
        .unwrap();
        let updated = EncryptionState::enabled(KeyProviderConfig::File {
            path: "/new.key".into(),
        });
        write_encryption_state_durable(dir.path(), &updated).unwrap();
        assert_eq!(read_encryption_state(dir.path()).unwrap(), Some(updated));
    }
}
