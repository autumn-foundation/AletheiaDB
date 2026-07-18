//! String interner persistence.
//!
//! This module handles the serialization and deserialization of the [`GLOBAL_INTERNER`].
//! The interner is critical for the database's operation as it maps all string labels and property keys to
//! compact integer IDs.
//!
//! # Format
//!
//! The interner is saved as a Bitcode-encoded [`StringInternerData`] struct,
//! followed by a 4-byte CRC32 checksum.
//!
//! ```text
//! [ Bitcode Encoded Data ] [ CRC32 (4 bytes) ]
//! ```
//!
//! # Safety
//!
//! Restoring the string interner **must** be the first step during database recovery. All other
//! persistence files (like vector indexes or adjacency lists) refer to strings by their interned IDs.
//! If the interner is not restored to the exact same state, these IDs will map to incorrect strings,
//! causing data corruption.

use crate::core::{GLOBAL_INTERNER, InternedString};
use std::path::Path;
use std::sync::Arc;

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    GraphIndexData, PersistedPropertyMap, PersistedPropertyValue, PersistedVersionType,
    StringInternerData, TemporalAdjacencyData, TemporalIndexData,
};
use super::{INTERNER_MAGIC, MANIFEST_VERSION};
use crate::encryption::cipher::Cipher;

/// Sentinel live id used when a persisted file-space interner id cannot be
/// mapped through the remap (i.e. it references a position past the end of the
/// saved interner file — genuine on-disk corruption). It is never a valid
/// interner id in practice, so downstream resolution of a property key / string
/// value / label stamped with it deterministically *fails* (the node/edge is
/// skipped and counted as a load error) rather than silently resolving to some
/// unrelated live string. This preserves corruption detection while the remap
/// eliminates the benign file-id vs live-id divergence that Issue #3490 was.
const UNMAPPABLE_FILE_ID: u32 = u32::MAX;

/// Translation table from **persisted file-space interner ids** to **live
/// interner ids**, produced by [`restore_string_interner`].
///
/// # Why this exists (Issue #3490)
///
/// Persistence stores raw `u32` interner ids for property keys, string values,
/// and node/edge labels. Those ids are assigned by the process-global
/// [`GLOBAL_INTERNER`] in first-intern order, so they are only meaningful
/// relative to the interner state that produced them. The saved interner file
/// records the strings in that id order (position `i` == the string that had
/// interner id `i` at save time).
///
/// On reload the live interner is generally **not** pristine, so re-interning
/// the saved strings yields *different* live ids than the file positions they
/// were saved at. Two mechanisms make the live interner diverge from the saved
/// file order before [`restore_string_interner`] runs:
///
/// 1. **WAL bootstrap deserialization.** During startup, `wal.read_from`
///    (`src/db/config.rs`) deserializes the on-disk WAL entries, and
///    `PropertyMap::deserialize` (`src/core/property/map.rs`) interns every
///    property **key** it decodes into the process-global interner. This
///    happens *before* the saved interner file is restored, so the keys land at
///    whatever ids the interner hands out next — not the ids they had when the
///    interner file was written. (WAL bootstrap is a raw deserialize, not a
///    differential replay: the differential WAL replay proper runs *after*
///    `load_indexes_startup`, and it does not intern.)
/// 2. **The interner is a process-global singleton.** `GLOBAL_INTERNER` is
///    shared across every `AletheiaDB` instance and every `open()` in the
///    process, so any prior or concurrent population (earlier opens, other
///    instances) leaves it already holding strings at ids unrelated to any one
///    saved file.
///
/// Either way, the string at file position `i` re-interns to some live id that
/// is generally not `i`. The old restore path asserted positional identity and
/// aborted with an `InternerMismatch`, which was swallowed to a warning and
/// silently abandoned the whole graph index — dropping data.
///
/// The fix decouples the two id namespaces: the saved interner file defines a
/// **file-local** id space, and this remap translates each persisted file id to
/// the live id the same string actually has in this process. Every persisted
/// `u32` interner id (keys, string values, labels, `removed_keys`) must be
/// routed through [`InternerRemap::remap_id`] before being resolved against the
/// live interner. The on-disk wire format is unchanged — this is purely a
/// load-time interpretation fix.
///
/// # Positional-identity invariant
///
/// The remap assumes `data.strings[i]` is exactly the string whose live id was
/// `i` when the interner file was saved. That holds only because
/// [`StringInterner::get_all_strings`](crate::core::interning::StringInterner::get_all_strings)
/// (`src/core/interning.rs`) `.flatten()`s the id-ordered slot table, and the
/// interner only ever leaves **tail** holes (capacity rollbacks), never
/// interior ones — so flattening drops only trailing empty slots and preserves
/// the position == id coupling for every retained string. If that ever changes
/// (interior holes), this remap and `get_all_strings` would need to co-evolve.
///
/// An [`InternerRemap::identity`] variant (used when no interner file was
/// loaded) passes ids through unchanged, exactly reproducing legacy behavior.
#[derive(Debug, Clone, Default)]
pub struct InternerRemap {
    /// `mapping[file_position]` == the live [`InternedString`] the saved string
    /// at that position resolves to in this process. `None` == identity remap
    /// (no translation; every id passes through unchanged).
    mapping: Option<Vec<InternedString>>,
}

impl InternerRemap {
    /// An identity remap: every persisted id is used verbatim as a live id.
    ///
    /// Used when there is no saved interner file to translate against (e.g. a
    /// first run, or best-effort recovery where the interner file is absent),
    /// preserving the exact pre-#3490 behavior for those paths.
    pub fn identity() -> Self {
        Self { mapping: None }
    }

    /// Build a remap from the saved strings, in file order.
    ///
    /// `positions[i]` is the live id that the string at file position `i`
    /// resolves to after being interned into the current [`GLOBAL_INTERNER`].
    fn from_positions(positions: Vec<InternedString>) -> Self {
        Self {
            mapping: Some(positions),
        }
    }

    /// Translate a persisted file-space interner id to its live id.
    ///
    /// Returns `None` when a mapping remap does not cover `file_id` (the id
    /// points past the end of the saved interner file — genuine corruption).
    /// Identity remaps always return `Some` (the id verbatim).
    pub fn resolve(&self, file_id: u32) -> Option<InternedString> {
        match &self.mapping {
            None => Some(InternedString::from_raw(file_id)),
            Some(positions) => positions.get(file_id as usize).copied(),
        }
    }

    /// Translate a persisted file-space id to a live id as a raw `u32`,
    /// substituting [`UNMAPPABLE_FILE_ID`] for an out-of-range (corrupt) id so
    /// that later resolution fails loudly instead of returning wrong data.
    fn remap_id(&self, file_id: u32) -> u32 {
        self.resolve(file_id)
            .map(|id| id.as_u32())
            .unwrap_or(UNMAPPABLE_FILE_ID)
    }

    /// Rewrite every interner id embedded in a persisted property map from
    /// file space to live space (property keys and any `String` values).
    fn remap_property_map(&self, map: &mut PersistedPropertyMap) {
        for (key_idx, value) in map.entries.iter_mut() {
            *key_idx = self.remap_id(*key_idx);
            if let PersistedPropertyValue::String(value_idx) = value {
                *value_idx = self.remap_id(*value_idx);
            }
        }
    }

    /// Rewrite every persisted interner id in a loaded [`GraphIndexData`] in
    /// place, so the existing restore path (which resolves ids against the live
    /// interner) sees live ids. Covers node/edge labels and node/edge property
    /// maps (keys + string values).
    ///
    /// An identity remap leaves the data byte-for-byte unchanged.
    pub fn remap_graph_index_data(&self, data: &mut GraphIndexData) {
        if self.mapping.is_none() {
            return;
        }
        for node in data.nodes.iter_mut() {
            node.label_idx = self.remap_id(node.label_idx);
            self.remap_property_map(&mut node.properties);
        }
        for edge in data.edges.iter_mut() {
            edge.label_idx = self.remap_id(edge.label_idx);
            self.remap_property_map(&mut edge.properties);
        }
    }

    /// Rewrite every persisted interner id in a loaded [`TemporalIndexData`] in
    /// place. Covers node/edge version labels, version property maps (keys +
    /// string values), the `removed_keys` of delta versions, and the anchor
    /// `full_state` property maps.
    ///
    /// An identity remap leaves the data byte-for-byte unchanged.
    pub fn remap_temporal_index_data(&self, data: &mut TemporalIndexData) {
        if self.mapping.is_none() {
            return;
        }
        for version in data.node_versions.iter_mut() {
            version.label_idx = self.remap_id(version.label_idx);
            self.remap_property_map(&mut version.properties);
            if let PersistedVersionType::Delta { removed_keys, .. } = &mut version.version_type {
                for key_idx in removed_keys.iter_mut() {
                    *key_idx = self.remap_id(*key_idx);
                }
            }
        }
        for version in data.edge_versions.iter_mut() {
            version.label_idx = self.remap_id(version.label_idx);
            self.remap_property_map(&mut version.properties);
            if let PersistedVersionType::Delta { removed_keys, .. } = &mut version.version_type {
                for key_idx in removed_keys.iter_mut() {
                    *key_idx = self.remap_id(*key_idx);
                }
            }
        }
        for anchor in data.node_anchors.iter_mut() {
            self.remap_property_map(&mut anchor.full_state);
        }
        for anchor in data.edge_anchors.iter_mut() {
            self.remap_property_map(&mut anchor.full_state);
        }
    }

    /// Rewrite every persisted edge-type interner id in a loaded
    /// [`TemporalAdjacencyData`] in place. Each `PersistedTemporalAdjacencyEntry`
    /// stores its edge-type `label` as a file-space interner id; this translates
    /// it to the live id so the #3225 AS-OF temporal-traversal edge-type filter
    /// (which compares the entry `label` against a live [`InternedString`])
    /// matches the correct edge type after reload (Issue #3490).
    ///
    /// An unmappable (corrupt/out-of-range) file id becomes
    /// [`UNMAPPABLE_FILE_ID`], which fails the live-interner resolve check in
    /// the reconstruction step (that entry is skipped and counted loudly)
    /// rather than being stored as a garbage edge type.
    ///
    /// An identity remap leaves the data byte-for-byte unchanged.
    pub fn remap_temporal_adjacency_data(&self, data: &mut TemporalAdjacencyData) {
        if self.mapping.is_none() {
            return;
        }
        for node_entry in data.outgoing.iter_mut() {
            for entry in node_entry.entries.iter_mut() {
                entry.label = self.remap_id(entry.label);
            }
        }
    }
}

/// Save the global string interner to disk with CRC32 checksum using atomic write.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::index_persistence::strings::save_string_interner;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let path = dir.path().join("interner.idx");
///
/// save_string_interner(&path).unwrap();
/// assert!(path.exists());
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be created or written to.
/// - The temporary file cannot be renamed to the target path.
pub fn save_string_interner(path: &Path) -> Result<()> {
    save_string_interner_with_cipher(path, None)
}

/// Save the global string interner, optionally encrypting it at rest
/// (Issue #481). With `cipher == None` this is identical to
/// [`save_string_interner`].
pub fn save_string_interner_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<()> {
    let strings = GLOBAL_INTERNER.get_all_strings();

    let data = StringInternerData {
        magic: INTERNER_MAGIC,
        version: MANIFEST_VERSION,
        string_count: strings.len() as u64,
        strings,
    };

    super::common::save_encoded_maybe_encrypted(&data, path, cipher)
}

/// Save the global string interner, encrypting with the current generation of
/// an [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
///
/// Returns the exact number of strings written, so the manifest's
/// `string_count` describes precisely what is on disk rather than a later,
/// racy `GLOBAL_INTERNER.len()` sample (configurable interner cap fix).
pub(crate) fn save_string_interner_with_keyring(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<u64> {
    let strings = GLOBAL_INTERNER.get_all_strings();
    let string_count = strings.len() as u64;

    let data = StringInternerData {
        magic: INTERNER_MAGIC,
        version: MANIFEST_VERSION,
        string_count,
        strings,
    };

    super::common::save_encoded_maybe_encrypted_with_keyring(&data, path, keyring)?;
    Ok(string_count)
}

/// Load the string interner from disk and validate CRC32 checksum.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::index_persistence::strings::{save_string_interner, load_string_interner};
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let path = dir.path().join("interner.idx");
///
/// save_string_interner(&path).unwrap();
///
/// let data = load_string_interner(&path).unwrap();
/// assert_eq!(data.version, aletheiadb::storage::index_persistence::MANIFEST_VERSION);
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - The file does not exist or cannot be read.
/// - The file size exceeds [`MAX_STRING_INTERNER_FILE_SIZE`](super::MAX_STRING_INTERNER_FILE_SIZE).
/// - The CRC32 checksum does not match the data.
/// - The magic bytes or version are invalid.
/// - The string count exceeds [`MAX_STRING_COUNT`](super::MAX_STRING_COUNT) or length exceeds [`MAX_STRING_LENGTH`](super::MAX_STRING_LENGTH) (DoS protection).
pub fn load_string_interner(path: &Path) -> Result<StringInternerData> {
    load_string_interner_with_cipher(path, None)
}

/// Load the string interner, transparently decrypting it if it was written
/// encrypted (Issue #481). A legacy plaintext interner is loaded even when a
/// cipher is supplied (header sniffing).
///
/// # Errors
///
/// Same as [`load_string_interner`], plus a structured error if the file is
/// encrypted but no cipher is supplied or decryption fails.
pub fn load_string_interner_with_cipher(
    path: &Path,
    cipher: Option<&Arc<dyn Cipher>>,
) -> Result<StringInternerData> {
    let data: StringInternerData = super::common::load_encoded_maybe_encrypted(
        path,
        super::MAX_STRING_INTERNER_FILE_SIZE,
        "String interner",
        cipher,
    )?;
    validate_string_interner(data, path)
}

/// Load the string interner, decrypting via an
/// [`IndexKeyring`](super::common::IndexKeyring) that dispatches on the header
/// `key_version` (Issue #488 key rotation).
pub(crate) fn load_string_interner_with_keyring(
    path: &Path,
    keyring: Option<&super::common::IndexKeyring>,
) -> Result<StringInternerData> {
    let data: StringInternerData = super::common::load_encoded_maybe_encrypted_with_keyring(
        path,
        super::MAX_STRING_INTERNER_FILE_SIZE,
        "String interner",
        keyring,
    )?;
    validate_string_interner(data, path)
}

/// Validate a decoded string-interner payload (magic, version, DoS limits).
fn validate_string_interner(data: StringInternerData, path: &Path) -> Result<StringInternerData> {
    // Validate magic bytes
    if data.magic != INTERNER_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: INTERNER_MAGIC,
            got: data.magic,
        });
    }

    // Validate version
    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    // Validate string count to prevent DoS via memory exhaustion.
    //
    // The effective limit honors the runtime cap configured at `open()` from
    // `persistence.max_interned_strings` (set on GLOBAL_INTERNER before indexes
    // load), floored by the generous `MAX_STRING_COUNT` backstop. This keeps the
    // two caps in lockstep: a database that raised the runtime cap and interned
    // past the old default reopens cleanly instead of being rejected here.
    let effective_max = super::MAX_STRING_COUNT.max(GLOBAL_INTERNER.max_capacity() as u64);
    if data.string_count > effective_max {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "String count {} exceeds maximum allowed count {}",
                data.string_count, effective_max
            ),
        });
    }

    // Validate individual string lengths to prevent DoS
    for s in &data.strings {
        if s.len() > super::MAX_STRING_LENGTH {
            return Err(IndexPersistenceError::SizeLimitExceeded {
                message: format!(
                    "String length {} exceeds maximum allowed length {}",
                    s.len(),
                    super::MAX_STRING_LENGTH
                ),
            });
        }
    }

    Ok(data)
}

/// Restore the persisted strings into `GLOBAL_INTERNER`, returning a
/// [`InternerRemap`] that translates persisted file-space ids to live ids.
///
/// This must be called before loading any other indexes since they reference
/// string indices — but, crucially (Issue #3490), the caller must translate
/// every persisted `u32` interner id through the returned [`InternerRemap`]
/// (via [`InternerRemap::remap_graph_index_data`] /
/// [`InternerRemap::remap_temporal_index_data`]) rather than resolving raw
/// persisted ids directly against the live interner.
///
/// # File-id vs live-id contract
///
/// Each saved string is re-interned into the (possibly already-populated) live
/// interner. Because WAL replay may have interned property keys in a different
/// order before this runs, the live id assigned to the string at file position
/// `i` is generally **not** `i`. This function records `file_position -> live
/// id` for every saved string and returns it as the remap. It does **not**
/// assert positional identity (the pre-#3490 behavior that spuriously failed
/// with `InternerMismatch` and silently dropped the graph index).
///
/// Empty strings are interned like any other string; a saved `""` is a real
/// interned empty string (holes are already filtered out by
/// [`StringInterner::get_all_strings`](crate::core::interning::StringInterner::get_all_strings)
/// before save), so its file position still needs a live mapping in case data
/// references it.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::index_persistence::strings::{save_string_interner, load_string_interner, restore_string_interner};
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let path = dir.path().join("interner.idx");
///
/// // Save current state
/// save_string_interner(&path).unwrap();
///
/// // Load data
/// let data = load_string_interner(&path).unwrap();
///
/// // Restore (re-interns all strings) and obtain the file-id -> live-id remap.
/// let remap = restore_string_interner(&data).unwrap();
/// let _ = remap; // apply to loaded graph/temporal indexes before resolving ids
/// ```
///
/// # Errors
///
/// Currently infallible (returns `Ok`): persisted strings are re-admitted via
/// `intern_unchecked`, which bypasses the runtime capacity cap, so a reopen
/// under a cap lower than the persisted count loads cleanly instead of wedging.
/// The `Result` return is retained for signature stability with the callers'
/// `?` chains and to leave room for future validation.
pub fn restore_string_interner(data: &StringInternerData) -> Result<InternerRemap> {
    let mut positions = Vec::with_capacity(data.strings.len());
    for s in &data.strings {
        // Re-admit persisted strings via `intern_unchecked`, bypassing the
        // runtime capacity cap (configurable interner cap fix). These strings
        // were already admitted once when they were written, so restoring
        // known-good on-disk data must NEVER be blocked by a runtime cap that is
        // lower than the persisted count — that would wedge the whole reopen with
        // a confusing "Failed to intern string" mid-load. The load-validation cap
        // (`validate_string_interner`) still bounds the persisted COUNT up front,
        // so a genuinely oversized file is rejected before we reach here; NEW
        // interns on the live write path remain capped as normal.
        let interned_id = GLOBAL_INTERNER.intern_unchecked(s);
        // Record file_position (== `positions.len()`) -> live interner id. We do
        // NOT require live id == file position: the live interner may already
        // hold these strings (e.g. from WAL replay) at different ids. The remap
        // captures whatever translation is needed so persisted ids resolve to
        // the correct strings regardless of interning order (Issue #3490).
        positions.push(interned_id);
    }
    Ok(InternerRemap::from_positions(positions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crc32fast::Hasher;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_string_interner_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Intern some strings
        let _idx1 = GLOBAL_INTERNER.intern("test_string_1").unwrap();
        let _idx2 = GLOBAL_INTERNER.intern("test_string_2").unwrap();
        let _idx3 = GLOBAL_INTERNER.intern("test_string_3").unwrap();

        // Save
        save_string_interner(&path).unwrap();

        // Load
        let loaded = load_string_interner(&path).unwrap();

        assert_eq!(loaded.magic, INTERNER_MAGIC);
        assert!(loaded.strings.contains(&"test_string_1".to_string()));
        assert!(loaded.strings.contains(&"test_string_2".to_string()));
        assert!(loaded.strings.contains(&"test_string_3".to_string()));
    }

    #[test]
    fn test_string_interner_encrypted_round_trip() {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;

        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");
        let cipher: Arc<dyn Cipher> = {
            let mut key = Zeroizing::new([0u8; 32]);
            key[1] = 0x33;
            Arc::new(Aes256GcmCipher::new(&key))
        };

        let s = format!("enc_str_{}", std::process::id());
        GLOBAL_INTERNER.intern(&s).unwrap();

        save_string_interner_with_cipher(&path, Some(&cipher)).unwrap();
        assert!(super::super::common::is_encrypted_index(
            &fs::read(&path).unwrap()
        ));

        let loaded = load_string_interner_with_cipher(&path, Some(&cipher)).unwrap();
        assert!(loaded.strings.contains(&s));

        // Fails closed without a cipher.
        assert!(load_string_interner_with_cipher(&path, None).is_err());
    }

    #[test]
    fn test_invalid_magic_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.idx");

        // Write garbage with valid CRC32
        let bad_data = StringInternerData {
            magic: *b"BAAD",
            version: 1,
            string_count: 0,
            strings: vec![],
        };
        let encoded = bitcode::encode(&bad_data);

        // Calculate valid CRC32 for the bad data
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut data_with_checksum = encoded;
        data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

        fs::write(&path, data_with_checksum).unwrap();

        // Should fail on magic validation
        let result = load_string_interner(&path);
        assert!(matches!(
            result,
            Err(IndexPersistenceError::InvalidMagic { .. })
        ));
    }

    #[test]
    fn test_crc_corruption_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Save valid data
        let _idx1 = GLOBAL_INTERNER.intern("corruption_test").unwrap();
        save_string_interner(&path).unwrap();

        // Corrupt the data (change a byte in the middle)
        let mut bytes = fs::read(&path).unwrap();
        bytes[10] ^= 0xFF; // Flip all bits in one byte
        fs::write(&path, bytes).unwrap();

        // Loading should fail with corruption error
        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }

    #[test]
    fn test_truncated_file_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Write a file that's too small
        fs::write(&path, b"ab").unwrap();

        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }

    #[test]
    fn test_string_count_limit_dos_protection() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Create data with excessive string count
        let bad_data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: super::super::MAX_STRING_COUNT + 1,
            strings: vec!["test".to_string()],
        };

        let encoded = bitcode::encode(&bad_data);

        // Calculate valid CRC32 for the bad data
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut data_with_checksum = encoded;
        data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

        fs::write(&path, data_with_checksum).unwrap();

        // Loading should fail with size limit error
        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Size limit exceeded"));
        assert!(err.to_string().contains("String count"));
    }

    /// Regression (configurable interner cap, finding 5): the load validator's
    /// effective cap is `max(MAX_STRING_COUNT, GLOBAL_INTERNER.max_capacity())`,
    /// so a persisted interner whose COUNT exceeds the 10M `MAX_STRING_COUNT`
    /// floor must be REJECTED at the default cap and LOAD only after the runtime
    /// cap is raised above it. This DISCRIMINATES the `max(...)` change (the old
    /// test used a 200K count, which is below the 10M floor and would pass on
    /// trunk, proving nothing).
    ///
    /// It mutates the process-global interner cap, so it runs in a SUBPROCESS to
    /// isolate the mutation from concurrent tests (mirrors the reopen test).
    #[test]
    fn string_count_above_floor_rejected_then_loads_under_raised_cap_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "storage::index_persistence::strings::tests::\
                 string_count_above_floor_rejected_then_loads_under_raised_cap_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for load-cap discrimination test");

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(status.success(), "load-cap discrimination helper failed");
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("load-cap discrimination helper did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn string_count_above_floor_rejected_then_loads_under_raised_cap_helper() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Header claims 15_000_000 strings — ABOVE the 10M MAX_STRING_COUNT floor.
        // The body carries a single string; validation rejects on the COUNT header
        // vs the effective cap, so the body size is irrelevant.
        let over_floor = super::super::MAX_STRING_COUNT + 5_000_000; // 15M
        assert!(over_floor > super::super::MAX_STRING_COUNT);
        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: over_floor,
            strings: vec!["loads_ok".to_string()],
        };
        let encoded = bitcode::encode(&data);
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut bytes = encoded;
        bytes.extend_from_slice(&checksum.to_le_bytes());
        fs::write(&path, bytes).unwrap();

        // At the default 10M floor: the 15M count is REJECTED (proves the load cap
        // is honored, not silently ignored).
        GLOBAL_INTERNER.set_max_capacity(super::super::MAX_STRING_COUNT as usize);
        let rejected = load_string_interner(&path);
        assert!(
            matches!(
                rejected,
                Err(IndexPersistenceError::SizeLimitExceeded { .. })
            ),
            "a count above the 10M floor must be rejected at the default cap, got: {rejected:?}"
        );

        // Raise the runtime cap ABOVE the persisted count: now it LOADS — proving
        // `validate_string_interner` uses `max(floor, configured cap)`, not just
        // the constant.
        GLOBAL_INTERNER.set_max_capacity(over_floor as usize + 1);
        let loaded =
            load_string_interner(&path).expect("count within the raised cap must load cleanly");
        assert_eq!(loaded.string_count, over_floor);
    }

    #[test]
    fn test_string_length_limit_dos_protection() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Create data with excessively long string
        // Use MAX_STRING_LENGTH + 1 to exceed the limit
        // Test file size limit is 20MB to allow this test to work
        let oversized_string = "x".repeat(super::super::MAX_STRING_LENGTH + 1);

        // Verify this exceeds the limit but is within file size bounds for test
        assert!(
            oversized_string.len() > super::super::MAX_STRING_LENGTH,
            "String should exceed MAX_STRING_LENGTH"
        );
        assert!(
            oversized_string.len() < super::super::MAX_STRING_INTERNER_FILE_SIZE as usize,
            "String should be within file size limit to test string length check"
        );

        let bad_data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 1,
            strings: vec![oversized_string],
        };

        let encoded = bitcode::encode(&bad_data);

        // Calculate valid CRC32 for the bad data
        let mut hasher = Hasher::new();
        hasher.update(&encoded);
        let checksum = hasher.finalize();
        let mut data_with_checksum = encoded;
        data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

        fs::write(&path, data_with_checksum).unwrap();

        // Loading should fail with size limit error
        let result = load_string_interner(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Size limit exceeded"));
        assert!(err.to_string().contains("String length"));
    }

    #[test]
    fn test_restore_string_interner_remaps_position_to_live_id() {
        // Issue #3490: the interner is a process-global pre-warmed singleton, so
        // a persisted string at file position 0 (e.g. "type", which warms to a
        // NON-zero COMMON_STRINGS id) does NOT re-intern back to id 0. The old
        // code failed this with `InternerMismatch`; the fix instead RETURNS a
        // remap translating file position -> live id, without error.
        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 1,
            strings: vec!["type".to_string()],
        };

        let remap = restore_string_interner(&data).expect("remap restore must not error");

        // Position 0 must resolve to the LIVE id of "type" (a warmed common
        // string, id != 0), proving the remap decouples file ids from live ids.
        let live = GLOBAL_INTERNER
            .get_id("type")
            .expect("'type' is pre-warmed");
        assert_eq!(
            remap.resolve(0),
            Some(live),
            "file position 0 must remap to the live id of \"type\""
        );
        assert_ne!(
            live.as_u32(),
            0,
            "precondition: pre-warmed \"type\" is not live id 0"
        );
    }

    #[test]
    fn test_restore_string_interner_remaps_new_string_to_live_id() {
        // A completely new (unique) string at file position 0 gets some live id
        // > 0 (the interner is pre-warmed). The remap must translate position 0
        // to that live id and it must resolve back to the same string.
        let unique_string = format!(
            "unique_string_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 1,
            strings: vec![unique_string.clone()],
        };

        let remap = restore_string_interner(&data).expect("remap restore must not error");

        let live = remap
            .resolve(0)
            .expect("file position 0 must map to a live id");
        assert!(
            live.as_u32() > 0,
            "new string must land at a live id > 0 (interner pre-warmed)"
        );
        assert_eq!(
            GLOBAL_INTERNER.resolve_with(live, |s| s.to_string()),
            Some(unique_string),
            "remapped live id must resolve back to the original string"
        );
    }

    #[test]
    fn test_interner_remap_identity_passthrough() {
        // An identity remap must pass every id through verbatim (legacy behavior
        // for the no-interner-file path).
        let remap = InternerRemap::identity();
        assert_eq!(remap.resolve(0), Some(InternedString::from_raw(0)));
        assert_eq!(remap.resolve(12345), Some(InternedString::from_raw(12345)));
    }

    #[test]
    fn test_interner_remap_out_of_range_is_unmappable() {
        // A referenced file id past the end of the saved interner file has no
        // mapping -> `resolve` returns None. This is genuine corruption and must
        // NOT silently resolve to some unrelated live string; downstream the id
        // is stamped UNMAPPABLE and the entity is skipped/errored.
        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: 2,
            strings: vec!["remap_a".to_string(), "remap_b".to_string()],
        };
        let remap = restore_string_interner(&data).expect("remap restore must not error");

        assert!(remap.resolve(0).is_some());
        assert!(remap.resolve(1).is_some());
        assert_eq!(
            remap.resolve(2),
            None,
            "an out-of-range (corrupt) file id must not map to any live id"
        );
        assert_eq!(remap.resolve(u32::MAX), None);
    }

    #[test]
    fn test_remap_graph_index_data_translates_polluted_order() {
        use crate::storage::index_persistence::graph::{
            new_graph_index_data, restore_property_map,
        };

        // Simulate save order: intern label + keys + a string value, capture the
        // FILE ids in that order, then build a persisted node referencing them.
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let label = format!("RemapLabel_{suffix}");
        let key_a = format!("remap_key_a_{suffix}");
        let key_b = format!("remap_key_b_{suffix}");
        let val = format!("remap_val_{suffix}");

        let file_label = GLOBAL_INTERNER.intern(&label).unwrap().as_u32();
        let file_key_a = GLOBAL_INTERNER.intern(&key_a).unwrap().as_u32();
        let file_key_b = GLOBAL_INTERNER.intern(&key_b).unwrap().as_u32();
        let file_val = GLOBAL_INTERNER.intern(&val).unwrap().as_u32();

        let mut graph = new_graph_index_data();
        graph.nodes.push(super::super::formats::PersistedNode {
            id: 1,
            label_idx: file_label,
            version_id: 1,
            properties: PersistedPropertyMap {
                entries: vec![
                    (file_key_a, PersistedPropertyValue::String(file_val)),
                    (file_key_b, PersistedPropertyValue::Int(7)),
                ],
            },
        });

        // Build a remap whose file positions map to DIFFERENT live ids than the
        // file ids above (mimicking WAL-replay pollution having shifted them).
        // The saved interner file lists strings in the id order captured above.
        // We can't easily control absolute live ids, so we assert behavior:
        // after remapping, the node's ids resolve to the ORIGINAL strings.
        let mut ordered = vec![String::new(); (file_val + 1) as usize];
        // Fill positions that our node references so `from_positions`-style
        // resolution is well-defined for them. Positions we don't reference are
        // left empty (harmless).
        ordered[file_label as usize] = label.clone();
        ordered[file_key_a as usize] = key_a.clone();
        ordered[file_key_b as usize] = key_b.clone();
        ordered[file_val as usize] = val.clone();
        let data = StringInternerData {
            magic: INTERNER_MAGIC,
            version: MANIFEST_VERSION,
            string_count: ordered.len() as u64,
            strings: ordered,
        };
        let remap = restore_string_interner(&data).unwrap();

        remap.remap_graph_index_data(&mut graph);

        // After remapping, the persisted ids are LIVE ids; restore must yield
        // the original strings intact.
        let node = &graph.nodes[0];
        assert_eq!(
            GLOBAL_INTERNER
                .resolve_with(InternedString::from_raw(node.label_idx), |s| s.to_string()),
            Some(label)
        );
        let props = restore_property_map(&node.properties).unwrap();
        assert_eq!(
            props.get(key_a.as_str()).and_then(|v| v.as_str()),
            Some(val.as_str())
        );
        assert!(matches!(
            props.get(key_b.as_str()),
            Some(crate::core::property::PropertyValue::Int(7))
        ));
    }
}
