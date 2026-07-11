//! WAL segment reader.
//!
//! This module provides a standalone function for reading WAL segments
//! from disk for recovery purposes.

use std::path::Path;
use std::sync::Arc;

use super::wal::segment_reader;
use super::wal::{LSN, WalEntry};
use crate::core::error::Result;
use crate::encryption::cipher::Cipher;

/// Read WAL entries from a directory, starting from the specified LSN.
///
/// This is a standalone function that can be used for recovery without
/// requiring an active WAL writer. It reads all segment files in the directory
/// and parses entries that have LSN >= start_lsn.
///
/// # Arguments
///
/// * `wal_dir` - Path to the WAL directory containing segment files
/// * `start_lsn` - Only entries with LSN >= this value are returned
///
/// # Returns
///
/// A vector of WAL entries sorted by LSN.
pub fn read_wal_entries(wal_dir: &Path, start_lsn: LSN) -> Result<Vec<WalEntry>> {
    segment_reader::read_entries_from_dir(wal_dir, start_lsn)
}

/// Read WAL entries from a directory with an explicit crash-torn-tail recovery
/// policy (Issue #3433).
///
/// When `tolerate_torn_tail` is `true` (the default recovery behavior), an
/// undecodable trailing entry in the final segment stops replay there and the
/// intact prefix is applied; when `false`, any parse failure hard-errors
/// (fail-stop recovery). See
/// [`segment_reader::read_entries_from_dir_with_options`].
pub fn read_wal_entries_with_options(
    wal_dir: &Path,
    start_lsn: LSN,
    tolerate_torn_tail: bool,
) -> Result<Vec<WalEntry>> {
    read_wal_entries_with_cipher_and_options(wal_dir, start_lsn, None, tolerate_torn_tail)
}

/// Read WAL entries from a directory, decrypting encrypted segments with the
/// supplied `cipher`, and honoring the crash-torn-tail recovery policy.
///
/// This is the cipher-aware counterpart to [`read_wal_entries_with_options`].
/// Recovery replay for an encryption-at-rest database MUST route through here
/// with the configured WAL cipher: encrypted segments (versions 2/4/6/8/10)
/// cannot be decoded without it, so a cipher-less replay of an encrypted WAL
/// tail hard-errors (`Cannot read encrypted WAL segment ... without a cipher`)
/// and would otherwise brick startup after a crash that left acknowledged
/// writes in the WAL past the last index snapshot. Passing `None` reproduces
/// the plaintext behavior exactly.
pub fn read_wal_entries_with_cipher_and_options(
    wal_dir: &Path,
    start_lsn: LSN,
    cipher: Option<&Arc<dyn Cipher>>,
    tolerate_torn_tail: bool,
) -> Result<Vec<WalEntry>> {
    segment_reader::read_entries_from_dir_with_options(
        wal_dir,
        start_lsn,
        cipher,
        tolerate_torn_tail,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GLOBAL_INTERNER;
    use crate::core::id::NodeId;
    use crate::core::property::PropertyMap;
    use crate::core::temporal::time;
    use crate::storage::wal::DurabilityMode;
    use crate::storage::wal::WalOperation;
    use crate::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
    use tempfile::tempdir;

    fn create_op(id: u64) -> WalOperation {
        WalOperation::CreateNode {
            node_id: NodeId::new(id).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("ReaderNode{id}")).unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        }
    }

    /// Plaintext directory: `read_wal_entries_with_cipher_and_options` with
    /// `None` (and its `read_wal_entries_with_options` delegator) must return
    /// every durable entry. Exercises the plaintext-delegation branch of the
    /// new cipher-aware reader directly at the unit level (integration tests
    /// that also hit it live under `tests/`, which coverage ignores).
    #[test]
    fn cipher_and_options_reads_plaintext_with_none() {
        let dir = tempdir().unwrap();
        {
            let config = ConcurrentWalSystemConfig::new(dir.path())
                .with_durability_mode(DurabilityMode::Synchronous);
            let wal = ConcurrentWalSystem::new(config).unwrap();
            wal.append(create_op(1)).unwrap();
            wal.append(create_op(2)).unwrap();
            // drop → flush any remainder
        }

        let via_cipher_fn =
            read_wal_entries_with_cipher_and_options(dir.path(), LSN(1), None, true).unwrap();
        assert_eq!(
            via_cipher_fn.len(),
            2,
            "plaintext read via the cipher-aware function (None) must return both entries"
        );

        // The thin delegator must return the identical set.
        let via_delegator = read_wal_entries_with_options(dir.path(), LSN(1), true).unwrap();
        let lsns_a: Vec<u64> = via_cipher_fn.iter().map(|e| e.lsn.0).collect();
        let lsns_b: Vec<u64> = via_delegator.iter().map(|e| e.lsn.0).collect();
        assert_eq!(
            lsns_a, lsns_b,
            "delegator must match the cipher-aware reader"
        );
        assert_eq!(lsns_a, vec![1, 2]);
    }

    /// Encrypted directory: the reader MUST thread the supplied cipher through
    /// (decrypt path) — and MUST fail without it. Guards the fix that
    /// `ConcurrentWalSystem::read_from` no longer drops the cipher, and kills
    /// the mutant that would ignore the `cipher` argument.
    #[cfg(feature = "encryption")]
    #[test]
    fn cipher_and_options_decrypts_encrypted_and_requires_cipher() {
        use crate::encryption::key_provider::FileKeyProvider;
        use crate::encryption::{EncryptionConfig, EncryptionManager};

        let dir = tempdir().unwrap();
        let key_path = dir.path().join("mek.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();
        let manager =
            EncryptionManager::from_config(&EncryptionConfig::file_based(&key_path)).unwrap();
        let cipher = Arc::clone(manager.wal_cipher());

        {
            let mut config = ConcurrentWalSystemConfig::new(dir.path())
                .with_durability_mode(DurabilityMode::Synchronous);
            config.wal_cipher = Some(Arc::clone(&cipher));
            let wal = ConcurrentWalSystem::new(config).unwrap();
            wal.append(create_op(1)).unwrap();
            wal.append(create_op(2)).unwrap();
        }

        // With the cipher: entries decode.
        let decrypted =
            read_wal_entries_with_cipher_and_options(dir.path(), LSN(1), Some(&cipher), true)
                .unwrap();
        assert_eq!(
            decrypted.iter().map(|e| e.lsn.0).collect::<Vec<_>>(),
            vec![1, 2],
            "encrypted segments must decode when the cipher is threaded through"
        );

        // Without the cipher: encrypted segments cannot be read (proves the
        // cipher argument is load-bearing, not ignored).
        assert!(
            read_wal_entries_with_cipher_and_options(dir.path(), LSN(1), None, true).is_err(),
            "reading an encrypted WAL without a cipher must hard-error"
        );
    }
}
