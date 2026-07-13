//! API-key store: creation, verification, revocation, optional persistence
//! (Issue #3350).
//!
//! Security invariants:
//!
//! - Plaintext keys are returned exactly once at creation and **never
//!   stored** — the store holds only SHA-256 digests.
//! - Digest comparison uses [`subtle::ConstantTimeEq`]; verification scans
//!   every record without early exit on mismatch, so no string comparison of
//!   key material is timing-observable.
//! - The persisted JSON file contains digests (hex), never plaintext, is
//!   written atomically (temp file + rename) and chmodded `0600` on unix.
//! - `Debug` output contains counts only — no digests, no prefixes.

use crate::auth::Role;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use dashmap::DashMap;
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

/// Scheme prefix all generated API keys start with.
pub const KEY_SCHEME_PREFIX: &str = "aletheia_sk_";

/// How many characters of the random part are kept in the masked
/// [`Principal::key_prefix`] (out of 43 base64url characters).
const KEY_PREFIX_RANDOM_CHARS: usize = 8;

/// Random bytes per generated key (base64url-encoded into the key string).
const KEY_RANDOM_BYTES: usize = 32;

/// Maximum accepted length (in characters) of a key/principal display name.
const MAX_KEY_NAME_CHARS: usize = 256;

/// A named identity bound to a role. Never holds key material, so listing
/// principals is masked by construction.
///
/// Marked `#[non_exhaustive]`: future work (e.g. multi-tenancy, Issue #3365)
/// will add fields. Out-of-crate code obtains principals from
/// [`AuthStore`] operations rather than constructing them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Principal {
    /// Short random identifier (stable across restarts for persisted keys).
    pub id: String,
    /// Operator-chosen display name.
    pub name: String,
    /// The role deciding which access classes this principal may exercise.
    pub role: Role,
    /// Masked key prefix (scheme + first characters) for identification.
    pub key_prefix: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
}

impl Principal {
    /// Construct a synthetic, fully-privileged **anonymous** principal for
    /// surfaces running in explicit anonymous mode, where no credential is
    /// presented but a [`Principal`] value is still required (e.g. the
    /// autumn-web 0.5 server's class-parameterized auth extractor). Carries the
    /// `admin` role so it may exercise every access class, matching anonymous
    /// mode's documented "full, unauthenticated access". This is the only
    /// out-of-crate way to obtain a principal not backed by a stored key, since
    /// [`Principal`] is `#[non_exhaustive]`.
    ///
    // exposed for autumn-web migration (Issue #3524)
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            id: "anonymous".to_string(),
            name: "anonymous".to_string(),
            role: Role::Admin,
            key_prefix: String::new(),
            created_at: String::new(),
        }
    }
}

/// Errors from [`AuthStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// I/O failure reading or writing the persisted store.
    #[error("auth store I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The in-memory store could not be serialized for persistence.
    #[error("auth store serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// The persisted store file could not be parsed.
    #[error("invalid auth store file: {0}")]
    InvalidFile(String),
    /// Caller-supplied input was invalid.
    #[error("{0}")]
    InvalidInput(String),
}

/// One stored key: the digest plus its principal.
struct KeyRecord {
    /// SHA-256 digest of the full plaintext key string.
    digest: [u8; 32],
    principal: Principal,
    /// Whether this record is written to the persisted store. Bootstrap
    /// (env/config-supplied) keys are memory-only.
    persisted: bool,
}

/// On-disk representation of one key (digest only — NEVER plaintext).
#[derive(Serialize, Deserialize)]
struct PersistedKey {
    id: String,
    name: String,
    role: Role,
    /// Hex-encoded SHA-256 digest of the full key string.
    key_hash: String,
    key_prefix: String,
    created_at: String,
}

/// On-disk representation of the whole store.
#[derive(Serialize, Deserialize)]
struct PersistedStore {
    version: u32,
    keys: Vec<PersistedKey>,
}

const PERSIST_FORMAT_VERSION: u32 = 1;

/// Thread-safe store of API keys, keyed by the SHA-256 digest of the full
/// key string. See the module docs for the security invariants.
pub struct AuthStore {
    /// Records keyed by principal id (revocation is by id).
    records: DashMap<String, KeyRecord>,
    /// When `Some`, admin mutations are persisted to this JSON file.
    persist_path: Option<PathBuf>,
    /// Serializes snapshot+write cycles so concurrent saves can't interleave.
    save_lock: parking_lot::Mutex<()>,
}

impl std::fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Counts only: no key material, no hashes, no prefixes.
        f.debug_struct("AuthStore")
            .field("keys", &self.records.len())
            .field("persistent", &self.persist_path.is_some())
            .finish()
    }
}

impl Default for AuthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthStore {
    /// Create an empty, memory-only store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            records: DashMap::new(),
            persist_path: None,
            save_lock: parking_lot::Mutex::new(()),
        }
    }

    /// Create a store persisted at `path`, loading existing keys if the file
    /// exists.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Io`] if the file exists but cannot be read, or
    /// [`AuthError::InvalidFile`] if it cannot be parsed.
    pub fn with_persistence(path: impl AsRef<Path>) -> Result<Self, AuthError> {
        let path = path.as_ref().to_path_buf();
        let store = Self {
            records: DashMap::new(),
            persist_path: Some(path.clone()),
            save_lock: parking_lot::Mutex::new(()),
        };
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            let parsed: PersistedStore = serde_json::from_str(&contents)
                .map_err(|e| AuthError::InvalidFile(e.to_string()))?;
            if parsed.version != PERSIST_FORMAT_VERSION {
                return Err(AuthError::InvalidFile(format!(
                    "unsupported auth store version {} (expected {})",
                    parsed.version, PERSIST_FORMAT_VERSION
                )));
            }
            for key in parsed.keys {
                let digest = hex_decode_32(&key.key_hash).ok_or_else(|| {
                    AuthError::InvalidFile(format!("invalid key hash for id '{}'", key.id))
                })?;
                let principal = Principal {
                    id: key.id.clone(),
                    name: key.name,
                    role: key.role,
                    key_prefix: key.key_prefix,
                    created_at: key.created_at,
                };
                store.records.insert(
                    key.id,
                    KeyRecord {
                        digest,
                        principal,
                        persisted: true,
                    },
                );
            }
        }
        Ok(store)
    }

    /// Create a new key with the given display name and role.
    ///
    /// Returns the principal plus the plaintext key — **exactly once**. The
    /// plaintext is never stored; only its SHA-256 digest is retained.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidInput`] for an empty name or a name
    /// longer than 256 characters, or a persistence error if the store is
    /// file-backed and saving fails (in which case the key is rolled back
    /// and does not take effect).
    pub fn create_key(
        &self,
        name: &str,
        role: Role,
    ) -> Result<(Principal, Zeroizing<String>), AuthError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AuthError::InvalidInput(
                "key name must not be empty".to_string(),
            ));
        }
        if name.chars().count() > MAX_KEY_NAME_CHARS {
            return Err(AuthError::InvalidInput(format!(
                "key name must be at most {MAX_KEY_NAME_CHARS} characters"
            )));
        }

        let mut random_bytes = [0u8; KEY_RANDOM_BYTES];
        OsRng.fill_bytes(&mut random_bytes);
        let mut random_part = URL_SAFE_NO_PAD.encode(random_bytes);
        random_bytes.zeroize();

        let key = Zeroizing::new(format!("{KEY_SCHEME_PREFIX}{random_part}"));
        // The random part is pure ASCII base64url, so byte slicing is safe.
        let key_prefix = format!(
            "{KEY_SCHEME_PREFIX}{}",
            &random_part[..KEY_PREFIX_RANDOM_CHARS.min(random_part.len())]
        );
        random_part.zeroize();

        let principal = Principal {
            id: self.generate_unique_id(),
            name: name.to_string(),
            role,
            key_prefix,
            created_at: now_rfc3339(),
        };
        let record = KeyRecord {
            digest: digest_key(&key),
            principal: principal.clone(),
            persisted: true,
        };
        self.records.insert(principal.id.clone(), record);

        if let Err(e) = self.save() {
            // Keep memory and disk consistent: a key we couldn't persist
            // must not silently exist only until the next restart.
            self.records.remove(&principal.id);
            return Err(e);
        }
        Ok((principal, key))
    }

    /// List all principals, sorted by id. Masked by construction —
    /// [`Principal`] never holds key material.
    #[must_use]
    pub fn list(&self) -> Vec<Principal> {
        let mut principals: Vec<Principal> = self
            .records
            .iter()
            .map(|r| r.value().principal.clone())
            .collect();
        principals.sort_by(|a, b| a.id.cmp(&b.id));
        principals
    }

    /// Revoke a key by principal id. Takes effect for subsequent
    /// verifications immediately. Returns whether the id existed.
    ///
    /// # Errors
    ///
    /// Returns a persistence error if the store is file-backed and saving
    /// fails. The revocation still holds in memory (fail-secure): the key is
    /// gone for this process even if the file could not be updated.
    pub fn revoke(&self, id: &str) -> Result<bool, AuthError> {
        match self.records.remove(id) {
            None => Ok(false),
            Some((_, record)) => {
                if record.persisted {
                    self.save()?;
                }
                Ok(true)
            }
        }
    }

    /// Verify a presented key, returning its principal if (and only if) the
    /// SHA-256 digest of the presented string matches a stored digest.
    ///
    /// Constant-time discipline: every record is compared via
    /// [`subtle::ConstantTimeEq`] and the scan never exits early on a
    /// mismatch.
    #[must_use]
    pub fn verify(&self, presented: &str) -> Option<Principal> {
        let digest = digest_key(presented);
        let mut found: Option<Principal> = None;
        for entry in self.records.iter() {
            if bool::from(entry.value().digest.ct_eq(&digest)) {
                found = Some(entry.value().principal.clone());
            }
        }
        found
    }

    /// Install a bootstrap key supplied via env/config at startup.
    ///
    /// Bootstrap keys are **memory-only**: they are never written to the
    /// persisted store (the operator re-supplies them on each start).
    /// Idempotent for the same plaintext — re-inserting returns the existing
    /// principal instead of duplicating.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidInput`] for an empty name or plaintext.
    pub fn insert_bootstrap_key(
        &self,
        name: &str,
        role: Role,
        plaintext: &str,
    ) -> Result<Principal, AuthError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AuthError::InvalidInput(
                "bootstrap key name must not be empty".to_string(),
            ));
        }
        if plaintext.is_empty() {
            return Err(AuthError::InvalidInput(
                "bootstrap key must not be empty".to_string(),
            ));
        }

        let digest = digest_key(plaintext);
        // Idempotency: same plaintext already installed → return it.
        for entry in self.records.iter() {
            if bool::from(entry.value().digest.ct_eq(&digest)) {
                let existing = entry.value().principal.clone();
                if existing.role != role {
                    // Fail-closed: the requested role is IGNORED, the
                    // existing principal (and its role) is kept — a
                    // re-supplied bootstrap key can never escalate a key
                    // that already exists. Warn loudly so the operator
                    // notices the collision. NEVER log key material here.
                    eprintln!(
                        "WARNING: bootstrap key '{name}' matches an existing key belonging to \
                         principal '{}' with role '{}'; the requested role '{role}' is ignored \
                         and the existing principal is returned (no privilege change)",
                        existing.name, existing.role
                    );
                    #[cfg(feature = "http-server")]
                    tracing::warn!(
                        existing_principal = %existing.name,
                        existing_role = %existing.role,
                        requested_role = %role,
                        "bootstrap key matches an existing key with a different role; \
                         keeping the existing principal"
                    );
                }
                return Ok(existing);
            }
        }

        // Reveal at most 4 characters of an operator-chosen secret in the
        // masked prefix (generated keys expose 8 of 43 random chars, but a
        // bootstrap key may be low-entropy).
        let prefix: String = plaintext.chars().take(4).collect();
        let principal = Principal {
            id: self.generate_unique_id(),
            name: name.to_string(),
            role,
            key_prefix: format!("{prefix}\u{2026}"),
            created_at: now_rfc3339(),
        };
        self.records.insert(
            principal.id.clone(),
            KeyRecord {
                digest,
                principal: principal.clone(),
                persisted: false,
            },
        );
        Ok(principal)
    }

    /// Whether the store holds no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Number of keys in the store.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Generate a fresh random principal id not currently in use.
    fn generate_unique_id(&self) -> String {
        loop {
            let mut bytes = [0u8; 8];
            OsRng.fill_bytes(&mut bytes);
            let id = hex_encode(&bytes);
            if !self.records.contains_key(&id) {
                return id;
            }
        }
    }

    /// Persist the current set of `persisted` records, atomically
    /// (temp file + rename), with `0600` permissions on unix.
    ///
    /// A no-op for memory-only stores.
    fn save(&self) -> Result<(), AuthError> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        let _guard = self.save_lock.lock();

        let mut keys: Vec<PersistedKey> = self
            .records
            .iter()
            .filter(|r| r.value().persisted)
            .map(|r| {
                let p = &r.value().principal;
                PersistedKey {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    role: p.role,
                    key_hash: hex_encode(&r.value().digest),
                    key_prefix: p.key_prefix.clone(),
                    created_at: p.created_at.clone(),
                }
            })
            .collect();
        keys.sort_by(|a, b| a.id.cmp(&b.id));

        let serialized = serde_json::to_vec_pretty(&PersistedStore {
            version: PERSIST_FORMAT_VERSION,
            keys,
        })?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic write: write to a sibling temp file, fsync, then rename.
        let tmp_path = path.with_extension("tmp");
        // Remove any stale temp file first: `mode(0o600)` only applies when
        // the file is *created*, so a pre-existing temp file (e.g. from a
        // crashed save) could otherwise keep looser permissions.
        let _ = std::fs::remove_file(&tmp_path);
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp_path)?;
            file.write_all(&serialized)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
        // Make the rename itself durable: fsync the parent directory so the
        // new directory entry survives a crash (unix only; directories are
        // not opened for sync on other platforms).
        #[cfg(unix)]
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

/// SHA-256 digest of the full key string.
fn digest_key(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

/// Current wallclock time as RFC 3339 (UTC).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// Lowercase hex encoding.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            // Writing to a String cannot fail.
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Decode a 64-char hex string into 32 bytes. `None` on any malformation.
fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, chunk) in out.iter_mut().zip(s.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(chunk).ok()?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
