//! Durable per-subject key registry (Issue #3359, slice PR-1a).
//!
//! The keyring maps `subject_id -> SubjectEntry`, holding each subject's DEK in
//! **wrapped** (MEK-derived-cipher-encrypted) form. Erasing a subject sets
//! `wrapped_key = None` and rewrites the file so the wrapped blob is physically
//! removed from the live file — the random DEK is then unrecoverable.
//!
//! ## Durability & fail-closed load
//!
//! The registry persists as a bitcode+CRC [`SubjectKeyringSidecar`] written
//! through [`crate::db::rotation::write_durable`] (temp -> `sync_all` -> rename
//! -> parent-dir fsync). Load is **fail-closed**: a present-but-corrupt file
//! returns [`CryptoShredError::KeyringCorrupt`] (startup aborts) rather than
//! silently starting empty — the opposite of the tolerant snapshot-quarantine
//! policy, so an erased subject can never be silently re-exposed.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

use bitcode::{Decode, Encode};

use super::attestation::AttestationRecord;
use super::designation::DesignationTarget;
use super::error::CryptoShredError;
use super::subject::SubjectKey;

/// Initial (and current) subject key version.
pub const INITIAL_SUBJECT_KEY_VERSION: u32 = 1;

/// Current on-disk keyring sidecar format version.
pub const KEYRING_SIDECAR_VERSION: u16 = 1;

/// Max accepted keyring file size on load (DoS guard).
const MAX_KEYRING_BYTES: u64 = 64 * 1024 * 1024;

/// Filename of the durable keyring sidecar under the data root.
pub const KEYRING_FILENAME: &str = "subject_keyring.dat";

/// Filename of the in-flight-erase breadcrumb under the data root.
pub const BREADCRUMB_FILENAME: &str = "subject_erase.breadcrumb";

/// Lifecycle state of a subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum SubjectState {
    /// Designated; key present and usable.
    Active,
    /// Erased; key destroyed, payload permanently unrecoverable.
    Erased,
}

/// A subject's DEK in wrapped (encrypted) form.
///
/// Only ever the ciphertext of the DEK is persisted — never the raw key.
#[derive(Clone, PartialEq, Eq, Encode, Decode)]
pub struct WrappedKey {
    /// Version of the subject-wrapping key used to wrap this DEK.
    pub key_version: u32,
    /// AEAD wrap blob `[nonce || ciphertext || tag]` of the 32-byte DEK.
    pub wrapped: Vec<u8>,
}

impl std::fmt::Debug for WrappedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Defense-in-depth: never render the (key-adjacent) wrapped blob bytes,
        // only its length + the key version.
        f.debug_struct("WrappedKey")
            .field("key_version", &self.key_version)
            .field("wrapped_len", &self.wrapped.len())
            .finish()
    }
}

/// A durable registry entry for one subject.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct SubjectEntry {
    /// The subject identifier.
    pub subject_id: String,
    /// The wrapped DEK — `None` once erased (physically removed).
    pub wrapped_key: Option<WrappedKey>,
    /// The designation targets (entities / property keys) under this subject.
    pub designation: Vec<DesignationTarget>,
    /// Lifecycle state.
    pub state: SubjectState,
    /// Microseconds-since-epoch when the subject was first designated.
    pub created_at_micros: i64,
    /// Microseconds-since-epoch when the subject was erased (`None` if active).
    pub erased_at_micros: Option<i64>,
    /// The signed erasure attestation, recorded at erase time so a re-erase is
    /// an idempotent no-op returning the original attestation.
    pub attestation: Option<AttestationRecord>,
}

/// On-disk keyring wrapper (bitcode + CRC).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct SubjectKeyringSidecar {
    /// Format version for forward compatibility.
    pub version: u16,
    /// All subject entries.
    pub entries: Vec<SubjectEntry>,
}

/// In-memory per-subject key registry.
///
/// Holds the durable entries plus a small cache of unwrapped DEKs (cleared and
/// zeroized on erase). The cache is a pure optimization — every unwrapped key is
/// reconstructible from `entries` while the subject is `Active`.
#[derive(Default)]
pub struct SubjectKeyring {
    entries: BTreeMap<String, SubjectEntry>,
    /// Unwrapped-DEK cache; entries dropped (and thus zeroized) on erase.
    cache: HashMap<String, SubjectKey>,
}

impl std::fmt::Debug for SubjectKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never expose cached key material; report only counts + ids.
        f.debug_struct("SubjectKeyring")
            .field("subjects", &self.entries.len())
            .field("cached_keys", &self.cache.len())
            .finish()
    }
}

impl SubjectKeyring {
    /// An empty in-memory keyring.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of registered subjects.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the keyring has no subjects.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Borrow a subject entry, if present.
    #[must_use]
    pub fn get(&self, subject_id: &str) -> Option<&SubjectEntry> {
        self.entries.get(subject_id)
    }

    /// Insert or replace a subject entry.
    pub fn upsert(&mut self, entry: SubjectEntry) {
        self.entries.insert(entry.subject_id.clone(), entry);
    }

    /// Cache an unwrapped DEK for a subject.
    pub fn cache_key(&mut self, subject_id: &str, key: SubjectKey) {
        self.cache.insert(subject_id.to_string(), key);
    }

    /// Fetch a cached unwrapped DEK, if present.
    #[must_use]
    pub fn cached_key(&self, subject_id: &str) -> Option<&SubjectKey> {
        self.cache.get(subject_id)
    }

    /// Erase a subject in memory: drop (zeroize) any cached DEK, remove the
    /// wrapped blob, and flip the state to `Erased`, recording the attestation.
    ///
    /// The dropped `SubjectKey` zeroizes its bytes on drop.
    pub fn erase_in_memory(
        &mut self,
        subject_id: &str,
        erased_at_micros: i64,
        attestation: AttestationRecord,
    ) {
        // Dropping the cached key zeroizes it (SubjectKey wraps Zeroizing).
        self.cache.remove(subject_id);
        if let Some(entry) = self.entries.get_mut(subject_id) {
            entry.wrapped_key = None;
            entry.state = SubjectState::Erased;
            entry.erased_at_micros = Some(erased_at_micros);
            entry.attestation = Some(attestation);
        }
    }

    /// Force a subject to the erased state during crash recovery, creating a
    /// tombstone entry if the subject is not present. Fail-closed: a breadcrumbed
    /// subject is treated as erased no matter the on-disk keyring state.
    pub fn force_erased(&mut self, subject_id: &str, erased_at_micros: i64) {
        self.cache.remove(subject_id);
        match self.entries.get_mut(subject_id) {
            Some(entry) => {
                entry.wrapped_key = None;
                entry.state = SubjectState::Erased;
                if entry.erased_at_micros.is_none() {
                    entry.erased_at_micros = Some(erased_at_micros);
                }
            }
            None => {
                self.entries.insert(
                    subject_id.to_string(),
                    SubjectEntry {
                        subject_id: subject_id.to_string(),
                        wrapped_key: None,
                        designation: Vec::new(),
                        state: SubjectState::Erased,
                        created_at_micros: erased_at_micros,
                        erased_at_micros: Some(erased_at_micros),
                        attestation: None,
                    },
                );
            }
        }
    }

    /// Build the durable sidecar snapshot from the current entries.
    #[must_use]
    pub fn to_sidecar(&self) -> SubjectKeyringSidecar {
        SubjectKeyringSidecar {
            version: KEYRING_SIDECAR_VERSION,
            entries: self.entries.values().cloned().collect(),
        }
    }

    /// Replace all entries from a loaded sidecar (cache is left empty).
    fn load_from_sidecar(&mut self, sidecar: SubjectKeyringSidecar) {
        self.entries.clear();
        for entry in sidecar.entries {
            self.entries.insert(entry.subject_id.clone(), entry);
        }
    }
}

/// Encode a sidecar to `[bitcode || crc32 LE]` — the exact wire shape
/// [`crate::storage::index_persistence::common::load_encoded_with_crc`] reads,
/// so encoding here + writing through `write_durable` round-trips through the
/// standard loader.
fn encode_sidecar_with_crc(sidecar: &SubjectKeyringSidecar) -> Vec<u8> {
    let encoded = bitcode::encode(sidecar);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();
    let mut out = encoded;
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

/// Persist a keyring durably to `path` (temp -> `sync_all` -> rename ->
/// parent-dir fsync via `write_durable`).
///
/// # Errors
/// [`CryptoShredError::Io`] on any durable-write failure.
pub fn save_keyring(path: &Path, keyring: &SubjectKeyring) -> Result<(), CryptoShredError> {
    let bytes = encode_sidecar_with_crc(&keyring.to_sidecar());
    crate::db::rotation::write_durable(path, &bytes)
        .map_err(|e| CryptoShredError::Io(format!("keyring write failed: {e}")))
}

/// Load a keyring from `path`, **fail-closed**.
///
/// - Missing file -> empty keyring (`Ok`).
/// - Present + well-formed -> loaded (`Ok`).
/// - Present + corrupt / unknown version -> [`CryptoShredError::KeyringCorrupt`]
///   (`Err`) so startup aborts rather than silently re-exposing erased subjects.
pub fn load_keyring(path: &Path) -> Result<SubjectKeyring, CryptoShredError> {
    if !path.exists() {
        return Ok(SubjectKeyring::new());
    }
    let sidecar = crate::storage::index_persistence::common::load_encoded_with_crc::<
        SubjectKeyringSidecar,
    >(path, MAX_KEYRING_BYTES, "subject keyring")
    .map_err(|e| CryptoShredError::KeyringCorrupt(e.to_string()))?;

    if sidecar.version != KEYRING_SIDECAR_VERSION {
        return Err(CryptoShredError::KeyringCorrupt(format!(
            "unsupported keyring version {} (supported {})",
            sidecar.version, KEYRING_SIDECAR_VERSION
        )));
    }

    let mut keyring = SubjectKeyring::new();
    keyring.load_from_sidecar(sidecar);
    Ok(keyring)
}

/// Write the in-flight-erase breadcrumb naming `subject_id`, durably.
///
/// # Errors
/// [`CryptoShredError::Io`] on durable-write failure.
pub fn write_breadcrumb(path: &Path, subject_id: &str) -> Result<(), CryptoShredError> {
    crate::db::rotation::write_durable(path, subject_id.as_bytes())
        .map_err(|e| CryptoShredError::Io(format!("breadcrumb write failed: {e}")))
}

/// Read the breadcrumb's subject id, if the breadcrumb exists.
///
/// # Errors
/// [`CryptoShredError::Io`] if the breadcrumb is present but unreadable / not
/// valid utf-8 (fail-closed — do not silently ignore a present breadcrumb).
pub fn read_breadcrumb(path: &Path) -> Result<Option<String>, CryptoShredError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| CryptoShredError::Io("breadcrumb is not valid utf-8".to_string()))?;
            Ok(Some(s))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CryptoShredError::Io(format!("breadcrumb unreadable: {e}"))),
    }
}

/// Remove the breadcrumb and fsync its parent directory so the removal is
/// durable (a crash right after must not resurrect the breadcrumb).
pub fn clear_breadcrumb(path: &Path) {
    if std::fs::remove_file(path).is_ok()
        && let Some(parent) = path.parent()
    {
        crate::storage::index_persistence::fsync_dir(parent);
    }
}
