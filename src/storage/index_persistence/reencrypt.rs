//! Index-layer key-rotation re-encryption engine (Issue #488).
//!
//! Rebuilds every encrypted index file under a data directory from an old key
//! generation to a new one, in place, crash-safely, and resumably. It is the
//! real work behind key rotation: the counter-only
//! [`KeyRotationManager`](crate::encryption::KeyRotationManager) tracks the
//! version state machine, while this engine actually rewrites persisted bytes.
//!
//! # How it works
//!
//! Every encrypted index file is self-describing: Issue #481's 10-byte plaintext
//! header carries the `key_version` that wrote it. During a rotation the data
//! dir holds a MIX of old- and new-key files; the engine:
//!
//! 1. Holds BOTH generations in an [`IndexKeyring`](super::common::IndexKeyring)
//!    (so live reads dispatch on the header `key_version`).
//! 2. Walks the `indexes/` tree and, for each file still tagged with the OLD
//!    `key_version`, decrypts it with the old generation and re-encrypts it with
//!    the NEW generation, publishing atomically via temp-write + rename
//!    (a crash mid-file leaves either the intact old or the intact new file,
//!    never a torn one).
//! 3. Skips files already at the new `key_version` — the per-file header IS the
//!    progress marker, so a resumed pass converges idempotently.
//!
//! `complete` refuses while any old-key file remains; only then is the old
//! generation retired. `cancel` re-encrypts any already-migrated file back to
//! the old generation (a reverse pass) and retires the new generation, leaving
//! a uniform old-key dataset identical to the pre-`begin` state. Both
//! generations stay live for the whole operation, so reads never fail.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::atomic_write;
use super::common::{
    ENC_HEADER_LEN, IndexKeyring, decrypt_index_bytes, decrypt_index_bytes_with_keyring,
    encrypt_index_bytes_versioned, index_file_key_version, is_encrypted_index,
};
use crate::encryption::cipher::Cipher;

/// Errors from the index key-rotation engine.
#[derive(Debug, thiserror::Error)]
pub enum RotationError {
    /// Encryption is not configured (no cipher), so there is nothing to rotate.
    #[error("index encryption is not configured; cannot rotate keys")]
    NotConfigured,
    /// The new key derives the same index DEK as the old key.
    #[error("new key equals the current key; rotation would be a no-op and risk nonce reuse")]
    SameKey,
    /// Index-only key rotation was requested while one or more OTHER layers
    /// (WAL / checkpoint / cold storage) are still encrypted under the current
    /// MEK. Rotating only the index tree to a new MEK and then switching the key
    /// provider would leave those layers undecryptable — catastrophic loss — so
    /// v1 refuses (Issue #488 P0.1). Full-MEK rotation across every layer is a
    /// documented follow-up.
    #[error(
        "index-only key rotation is unsupported while other encrypted-at-rest \
         layers are present ({layers}); rotating the index alone to a new key \
         would leave those layers undecryptable"
    )]
    UnsupportedWhileEncryptedLayersPresent {
        /// Comma-separated names of the still-encrypted layers (e.g. "wal").
        layers: String,
    },
    /// A rotation is already in progress.
    #[error("a key rotation is already in progress")]
    AlreadyInProgress,
    /// No rotation is in progress.
    #[error("no key rotation is in progress")]
    NotInProgress,
    /// A file is stamped with the current (new) `key_version` but does NOT
    /// authenticate under the current key — evidence of a foreign key
    /// generation (e.g. an earlier interrupted rotation stamped the same version
    /// number with a *different* key). Treating it as already-migrated would let
    /// `complete` retire the old key over a file only the old key can read, so
    /// the rotation wedges here instead (Issue #488 P0.3).
    #[error(
        "index file {path} is stamped with the current key_version but does not \
         decrypt under the current key (foreign key generation); manual \
         intervention required"
    )]
    ForeignKeyVersionFile {
        /// The offending file.
        path: PathBuf,
    },
    /// `complete` was called while old-key files remain.
    #[error("cannot complete rotation: {remaining} index file(s) still hold the old key")]
    OldKeyFilesRemain {
        /// How many files are still tagged with a non-current key_version.
        remaining: usize,
    },
    /// A key provider failed while sourcing an MEK.
    #[error("key provider error: {0}")]
    KeyProvider(String),
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Index persistence error (decrypt/encrypt/serialize).
    #[error("index error: {0}")]
    Index(#[from] super::error::IndexPersistenceError),
}

/// Progress of a re-encryption pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RotationProgress {
    /// Total encrypted index files considered in this pass.
    pub files_total: usize,
    /// Files processed so far (re-encrypted + skipped).
    pub files_done: usize,
    /// Files re-encrypted to the new key so far.
    pub files_reencrypted: usize,
    /// Files skipped because they were already at the new key.
    pub files_skipped: usize,
}

/// Read-only classification of the on-disk index files by key generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RotationStatus {
    /// Encrypted files stamped with the current (new) `key_version`.
    pub at_current: usize,
    /// Encrypted files stamped with a non-current (old) `key_version`.
    pub at_old: usize,
    /// Encrypted files whose `key_version` matches no held generation.
    pub unknown: usize,
    /// Plaintext (non-encrypted) files encountered.
    pub plaintext: usize,
}

impl RotationStatus {
    /// True when no old-key or unknown-key encrypted files remain.
    #[must_use]
    pub fn is_fully_rotated(&self) -> bool {
        self.at_old == 0 && self.unknown == 0
    }
}

/// Result of the manifest-/index-body decrypt probe (Issue #3618).
///
/// [`RotationStatus`] classifies files by their 10-byte `AEIX` header
/// `key_version` ONLY; it never AEAD-decrypts the body. A wrong key that happens
/// to share the same `key_version` number therefore passes header classification
/// (`unknown == 0`) even though it cannot actually decrypt anything — a
/// false-PASS for `encryption verify`. This report captures an *active* decrypt
/// probe: it attempts to AEAD-decrypt representative index bodies with the live
/// keyring and records what actually authenticated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecryptProbeReport {
    /// Number of encrypted files whose body the probe actually attempted to
    /// decrypt (bounded — see [`IndexKeyRotation::verify_decryptable`]).
    pub probed: usize,
    /// Probed files whose body AEAD-decrypted successfully under the keyring.
    pub decrypted_ok: usize,
    /// Plaintext (non-`AEIX`) files encountered — skipped, NOT a failure.
    pub plaintext: usize,
    /// Paths of probed files whose body did NOT decrypt (wrong key material or
    /// corruption). Path only — never any key bytes.
    pub decrypt_failed: Vec<PathBuf>,
}

impl DecryptProbeReport {
    /// True when the probe attempted at least one decrypt and none failed.
    #[must_use]
    pub fn all_decrypted(&self) -> bool {
        self.decrypt_failed.is_empty()
    }
}

/// Per-file outcome of the byte-level re-encryption primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOutcome {
    Reencrypted,
    SkippedAlreadyNew,
    SkippedPlaintext,
}

/// The index re-encryption engine. Holds both key generations for the duration
/// of a rotation and rewrites files from the old generation to the new.
pub struct IndexKeyRotation {
    /// The `indexes/` directory whose encrypted files are rotated.
    indexes_dir: PathBuf,
    /// Shared keyring holding both generations (dispatches live reads/writes).
    keyring: IndexKeyring,
    /// Old (retiring) generation.
    old_version: u32,
    old_cipher: Arc<dyn Cipher>,
    /// New (current) generation.
    new_version: u32,
    new_cipher: Arc<dyn Cipher>,
}

impl IndexKeyRotation {
    /// Construct an engine over `indexes_dir` for a rotation from `old` to
    /// `new`. The `keyring` MUST already hold both generations (the new one
    /// current); the engine mutates it only at `complete`/`cancel` to retire a
    /// generation.
    pub(crate) fn new(
        indexes_dir: impl Into<PathBuf>,
        keyring: IndexKeyring,
        old_version: u32,
        old_cipher: Arc<dyn Cipher>,
        new_version: u32,
        new_cipher: Arc<dyn Cipher>,
    ) -> Self {
        Self {
            indexes_dir: indexes_dir.into(),
            keyring,
            old_version,
            old_cipher,
            new_version,
            new_cipher,
        }
    }

    /// The `key_version` new files are stamped with.
    #[must_use]
    pub fn new_version(&self) -> u32 {
        self.new_version
    }

    /// The retiring `key_version`.
    #[must_use]
    pub fn old_version(&self) -> u32 {
        self.old_version
    }

    /// Scan the index files and classify them by key generation without
    /// rewriting anything (dry-run / verify probe).
    pub fn status(&self) -> Result<RotationStatus, RotationError> {
        let mut status = RotationStatus::default();
        for path in self.enumerate_index_files()? {
            match peek_key_version(&path)? {
                None => status.plaintext += 1,
                Some(v) if v == self.new_version => status.at_current += 1,
                Some(v) if v == self.old_version => status.at_old += 1,
                Some(_) => status.unknown += 1,
            }
        }
        Ok(status)
    }

    /// Actively probe that index bodies decrypt under the live keyring
    /// (Issue #3618).
    ///
    /// [`status`](Self::status) classifies files by their `AEIX` header
    /// `key_version` alone and cannot catch a wrong key that shares the same
    /// version number (the body is never touched). This probe closes that hole:
    /// it AEAD-decrypts representative index *bodies* through the keyring and
    /// reports which authenticated. A wrong key (or a corrupted body) fails the
    /// AEAD auth tag — the header is AAD — so `decrypt_failed` becomes non-empty.
    ///
    /// **Bounded cost.** `verify` is an operator command, not a hot path, and
    /// decrypting every file of a huge database would be wasteful. The probe
    /// therefore decrypts the manifest body (the small canonical file every
    /// encrypted database writes) plus ONE representative encrypted file per
    /// distinct `key_version` actually present on disk. That fully catches a
    /// uniformly-wrong configured key — the Issue #3618 scenario — since a wrong
    /// key fails on the very first file of each generation.
    ///
    /// Plaintext (non-`AEIX`) files are counted under `plaintext` and skipped;
    /// they are not decrypt failures. Recorded failure paths never carry key
    /// material.
    pub fn verify_decryptable(&self) -> Result<DecryptProbeReport, RotationError> {
        use std::collections::HashSet;

        let mut report = DecryptProbeReport::default();
        let mut probed_versions: HashSet<u32> = HashSet::new();
        let manifest_path = self.indexes_dir.join("manifest.idx");

        // Walk EVERY non-scratch file (not just encrypted ones) so plaintext
        // files are counted rather than silently invisible. Sorted for a
        // deterministic representative per key-version.
        let mut files = Vec::new();
        if self.indexes_dir.exists() {
            collect_all_files(&self.indexes_dir, &mut files)?;
        }
        files.sort();

        for path in files {
            // verify may run against a LIVE db: a file listed a moment ago can be
            // removed/renamed by a concurrent writer's atomic_write between the
            // scan and this read. A vanished file is benign — skip it rather than
            // fail the probe; any OTHER io error still propagates.
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let Some(key_version) = index_file_key_version(&bytes) else {
                // Not an AEIX-encrypted file: plaintext, skip (not a failure).
                report.plaintext += 1;
                continue;
            };
            // Bounded cost: always probe the manifest body; otherwise probe only
            // the first file seen for each distinct key_version.
            let is_manifest = path == manifest_path;
            let first_of_version = probed_versions.insert(key_version);
            if !is_manifest && !first_of_version {
                continue;
            }
            report.probed += 1;
            match decrypt_index_bytes_with_keyring(&bytes, &path, Some(&self.keyring)) {
                Ok(_) => report.decrypted_ok += 1,
                // Path only — never any key bytes.
                Err(_) => report.decrypt_failed.push(path),
            }
        }
        Ok(report)
    }

    /// Re-encrypt every old-key file to the new key (forward pass).
    ///
    /// Idempotent and resumable: files already at the new key are skipped, so a
    /// crash-interrupted pass converges when re-run. `progress` is invoked after
    /// each file; returning `false` stops the pass early (used to simulate a
    /// crash mid-rotation in tests), leaving a valid mix that a later call
    /// finishes.
    pub fn re_encrypt(
        &self,
        progress: &mut dyn FnMut(RotationProgress) -> bool,
    ) -> Result<RotationProgress, RotationError> {
        let files = self.enumerate_index_files()?;
        let mut p = RotationProgress {
            files_total: files.len(),
            ..Default::default()
        };
        for path in files {
            match self.reencrypt_one(&path, self.new_version, &self.new_cipher)? {
                FileOutcome::Reencrypted => p.files_reencrypted += 1,
                FileOutcome::SkippedAlreadyNew | FileOutcome::SkippedPlaintext => {
                    p.files_skipped += 1
                }
            }
            p.files_done += 1;
            if !progress(p) {
                break;
            }
        }
        Ok(p)
    }

    /// Verify a full pass completed (zero old/unknown encrypted files remain)
    /// AND that every file claiming the new `key_version` actually authenticates
    /// under the new key.
    ///
    /// Binding verification to key IDENTITY, not just the version NUMBER, closes
    /// the double-rotation hole (Issue #488 P0.3): a file stamped with
    /// `new_version` by a *different* key (from an earlier interrupted rotation)
    /// is not silently counted as migrated — it wedges the rotation with
    /// [`RotationError::ForeignKeyVersionFile`] so [`complete`](Self::complete)
    /// can never retire the old key over a file only the old key can read.
    pub fn verify_complete(&self) -> Result<(), RotationError> {
        let mut remaining = 0;
        for path in self.enumerate_index_files()? {
            let bytes = std::fs::read(&path)?;
            match index_file_key_version(&bytes) {
                // Not an encrypted-index file (enumeration already excludes
                // these, but stay defensive).
                None => {}
                Some(v) if v == self.new_version => {
                    // Version says "new" — prove it with an AEAD auth check under
                    // the current key, not the bare number.
                    if decrypt_index_bytes(&bytes, &path, Some(&self.new_cipher)).is_err() {
                        return Err(RotationError::ForeignKeyVersionFile { path });
                    }
                }
                // Old or unknown key generation still present.
                Some(_) => remaining += 1,
            }
        }
        if remaining > 0 {
            return Err(RotationError::OldKeyFilesRemain { remaining });
        }
        Ok(())
    }

    /// Finish the rotation: require a fully-rotated dataset, then retire the old
    /// generation from the keyring so the old key can be dropped.
    pub fn complete(&self) -> Result<(), RotationError> {
        self.verify_complete()?;
        self.keyring.retain_only(self.new_version);
        Ok(())
    }

    /// Roll back: re-encrypt any already-migrated files back to the OLD key,
    /// then retire the new generation. Leaves a uniform old-key dataset. Both
    /// generations remain live until the reverse pass finishes, so reads stay
    /// correct throughout.
    pub fn cancel(&self) -> Result<(), RotationError> {
        for path in self.enumerate_index_files()? {
            // Reverse: rewrite files currently at the new key back to old.
            self.reencrypt_one(&path, self.old_version, &self.old_cipher)?;
        }
        self.keyring.retain_only(self.old_version);
        Ok(())
    }

    /// Byte-level, format-agnostic re-encryption of a single file to
    /// `target_version` using `target_cipher`. Reads the whole file, dispatches
    /// the decrypt cipher on the header `key_version` via the keyring, and
    /// publishes atomically. Files already at `target_version`, and plaintext
    /// files, are left untouched.
    fn reencrypt_one(
        &self,
        path: &Path,
        target_version: u32,
        target_cipher: &Arc<dyn Cipher>,
    ) -> Result<FileOutcome, RotationError> {
        let bytes = std::fs::read(path)?;
        if !is_encrypted_index(&bytes) {
            return Ok(FileOutcome::SkippedPlaintext);
        }
        let current = index_file_key_version(&bytes).unwrap_or(0);
        if current == target_version {
            // Key-identity binding (Issue #488 P0.3): a matching version NUMBER
            // is not proof this file was written by `target_cipher`. A prior
            // interrupted rotation could have stamped `target_version` with a
            // DIFFERENT key. Confirm the file authenticates under the target key
            // before trusting it as already-migrated; otherwise wedge rather
            // than skip, so a later `complete` never retires the old key over a
            // foreign-key file.
            return match decrypt_index_bytes(&bytes, path, Some(target_cipher)) {
                Ok(_) => Ok(FileOutcome::SkippedAlreadyNew),
                Err(_) => Err(RotationError::ForeignKeyVersionFile {
                    path: path.to_path_buf(),
                }),
            };
        }
        // Decrypt with whichever generation wrote this file (keyring dispatch).
        let plaintext = decrypt_index_bytes_with_keyring(&bytes, path, Some(&self.keyring))?;
        let reencrypted = encrypt_index_bytes_versioned(&plaintext, target_cipher, target_version)?;
        atomic_write(path, &reencrypted)?;
        Ok(FileOutcome::Reencrypted)
    }

    /// Recursively collect every regular file under `indexes_dir` that carries
    /// the encrypted-index header. Temp files (atomic-write scratch and the
    /// native-index decrypt scratch) are skipped by name.
    fn enumerate_index_files(&self) -> Result<Vec<PathBuf>, RotationError> {
        let mut out = Vec::new();
        if self.indexes_dir.exists() {
            collect_index_files(&self.indexes_dir, &mut out)?;
        }
        out.sort();
        Ok(out)
    }
}

/// Read only the header bytes of `path` and return its `key_version`, or `None`
/// for a plaintext file or a file too short to carry a header.
fn peek_key_version(path: &Path) -> Result<Option<u32>, RotationError> {
    use std::io::Read;
    let mut header = [0u8; ENC_HEADER_LEN];
    let mut file = std::fs::File::open(path)?;
    let mut filled = 0usize;
    loop {
        match file.read(&mut header[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if filled == header.len() {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(index_file_key_version(&header[..filled]))
}

/// A temp/scratch file the rotation engine must never treat as an index file.
fn is_scratch_file(name: &str) -> bool {
    name.ends_with(".tmp") || name.contains(".tmp.") || name.starts_with(".aeix-usearch-tmp-")
}

/// Recursively collect every regular, non-scratch file under `dir` — encrypted
/// AND plaintext. Unlike [`collect_index_files`], this does NOT filter to files
/// carrying the `AEIX` header, so the decrypt probe (Issue #3618) can count
/// plaintext files instead of treating them as absent.
fn collect_all_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), RotationError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_all_files(&path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_scratch_file(&name) {
            continue;
        }
        out.push(path);
    }
    Ok(())
}

fn collect_index_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), RotationError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_index_files(&path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_scratch_file(&name) {
            continue;
        }
        // Only files that are actually encrypted-index files participate.
        if peek_key_version(&path)?.is_some() {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::Aes256GcmCipher;
    use zeroize::Zeroizing;

    const OLD_V: u32 = 1;
    const NEW_V: u32 = 2;

    fn cipher_from_seed(seed: u8) -> Arc<dyn Cipher> {
        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = seed;
        key[1] = seed.wrapping_add(7);
        Arc::new(Aes256GcmCipher::new(&key))
    }

    /// A strict (per-version dispatch) keyring holding both generations.
    fn rotating_keyring(old: &Arc<dyn Cipher>, new: &Arc<dyn Cipher>) -> IndexKeyring {
        let ring = IndexKeyring::single_versioned(old.clone(), OLD_V);
        ring.add_generation(NEW_V, new.clone());
        ring
    }

    fn engine(dir: &Path, old: &Arc<dyn Cipher>, new: &Arc<dyn Cipher>) -> IndexKeyRotation {
        let keyring = rotating_keyring(old, new);
        IndexKeyRotation::new(dir, keyring, OLD_V, old.clone(), NEW_V, new.clone())
    }

    /// Write an encrypted index file at `path` with `key_version`, containing
    /// `plaintext` (CRC-appended so it round-trips like a real index file).
    fn write_enc(path: &Path, cipher: &Arc<dyn Cipher>, key_version: u32, plaintext: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let bytes = encrypt_index_bytes_versioned(plaintext, cipher, key_version).unwrap();
        atomic_write(path, &bytes).unwrap();
    }

    /// Decrypt an encrypted index file through a keyring (dispatch on header).
    fn read_enc(path: &Path, keyring: &IndexKeyring) -> Vec<u8> {
        let bytes = std::fs::read(path).unwrap();
        decrypt_index_bytes_with_keyring(&bytes, path, Some(keyring)).unwrap()
    }

    /// Lay down a realistic set of old-key (v1) encrypted files under
    /// `indexes/`, returning (path, plaintext) pairs.
    fn seed_old_files(indexes: &Path, old: &Arc<dyn Cipher>) -> Vec<(PathBuf, Vec<u8>)> {
        let files = vec![
            (indexes.join("manifest.idx"), b"MANIFEST-payload-0".to_vec()),
            (
                indexes.join("strings").join("interner.idx"),
                b"interner-payload-1".to_vec(),
            ),
            (
                indexes.join("graph").join("adjacency.idx"),
                b"graph-payload-2".to_vec(),
            ),
            (
                indexes.join("temporal").join("versions.idx"),
                b"temporal-payload-3".to_vec(),
            ),
            (
                indexes.join("vector").join("emb").join("meta.idx"),
                b"vector-meta-4".to_vec(),
            ),
            (
                indexes.join("vector").join("emb").join("current.usearch"),
                b"native-usearch-bytes-5".to_vec(),
            ),
        ];
        for (p, pt) in &files {
            write_enc(p, old, OLD_V, pt);
        }
        files
    }

    #[test]
    fn re_encrypt_advances_all_headers_and_preserves_data() {
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0x11), cipher_from_seed(0x22));
        let files = seed_old_files(&indexes, &old);

        let eng = engine(&indexes, &old, &new);
        let prog = eng.re_encrypt(&mut |_| true).unwrap();
        assert_eq!(prog.files_total, files.len());
        assert_eq!(prog.files_reencrypted, files.len());
        assert_eq!(prog.files_skipped, 0);

        // Every file now carries the new key_version and decrypts under new.
        let new_ring = IndexKeyring::single_versioned(new.clone(), NEW_V);
        for (p, pt) in &files {
            assert_eq!(peek_key_version(p).unwrap(), Some(NEW_V));
            assert_eq!(&read_enc(p, &new_ring), pt);
        }
        assert!(eng.status().unwrap().is_fully_rotated());
    }

    #[test]
    fn live_reads_dispatch_on_key_version_in_mixed_state() {
        // Rotate only SOME files, leaving a genuine mix, and assert reads via
        // the keyring return correct data for BOTH old- and new-key files.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0x33), cipher_from_seed(0x44));
        let files = seed_old_files(&indexes, &old);
        let keyring = rotating_keyring(&old, &new);

        // Stop after 3 files: a real mix of old and new on disk.
        let eng = IndexKeyRotation::new(
            &indexes,
            keyring.clone(),
            OLD_V,
            old.clone(),
            NEW_V,
            new.clone(),
        );
        let mut done = 0;
        eng.re_encrypt(&mut |_| {
            done += 1;
            done < 3
        })
        .unwrap();

        let status = eng.status().unwrap();
        assert!(
            status.at_current > 0 && status.at_old > 0,
            "expected a mix: {status:?}"
        );

        // Every file still reads correctly through the dispatching keyring,
        // regardless of which key encrypted it.
        for (p, pt) in &files {
            assert_eq!(
                &read_enc(p, &keyring),
                pt,
                "mixed-state read failed for {p:?}"
            );
        }
    }

    #[test]
    fn re_encrypt_is_idempotent_and_resumable() {
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0x55), cipher_from_seed(0x66));
        let files = seed_old_files(&indexes, &old);

        let eng = engine(&indexes, &old, &new);
        // First pass: stop after 2 files (simulated crash).
        let mut n = 0;
        eng.re_encrypt(&mut |_| {
            n += 1;
            n < 2
        })
        .unwrap();
        assert!(!eng.status().unwrap().is_fully_rotated());

        // Resume: re-run the same pass; it must finish and SKIP already-done.
        let prog = eng.re_encrypt(&mut |_| true).unwrap();
        assert!(
            prog.files_skipped >= 1,
            "resume must skip already-rotated files"
        );
        assert_eq!(prog.files_reencrypted + prog.files_skipped, files.len());
        assert!(eng.status().unwrap().is_fully_rotated());
        // A third pass is a pure no-op (all skipped) — idempotent.
        let again = eng.re_encrypt(&mut |_| true).unwrap();
        assert_eq!(again.files_reencrypted, 0);
        assert_eq!(again.files_skipped, files.len());
    }

    #[test]
    fn complete_refuses_until_full_pass_then_retires_old() {
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0x77), cipher_from_seed(0x88));
        seed_old_files(&indexes, &old);
        let keyring = rotating_keyring(&old, &new);
        let eng = IndexKeyRotation::new(
            &indexes,
            keyring.clone(),
            OLD_V,
            old.clone(),
            NEW_V,
            new.clone(),
        );

        // Partial: complete must refuse.
        let mut n = 0;
        eng.re_encrypt(&mut |_| {
            n += 1;
            n < 2
        })
        .unwrap();
        assert!(matches!(
            eng.complete().unwrap_err(),
            RotationError::OldKeyFilesRemain { .. }
        ));

        // Full pass, then complete succeeds and the old generation is retired.
        eng.re_encrypt(&mut |_| true).unwrap();
        eng.complete().unwrap();
        // After retiring old, the keyring no longer holds the old version.
        assert_eq!(keyring.current_version(), NEW_V);
        assert!(!keyring.has_version(OLD_V));
    }

    #[test]
    fn cancel_rolls_back_to_old_key() {
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0x99), cipher_from_seed(0xAA));
        let files = seed_old_files(&indexes, &old);
        let keyring = rotating_keyring(&old, &new);
        let eng = IndexKeyRotation::new(
            &indexes,
            keyring.clone(),
            OLD_V,
            old.clone(),
            NEW_V,
            new.clone(),
        );

        // Migrate a few, then cancel: all files return to the old key.
        let mut n = 0;
        eng.re_encrypt(&mut |_| {
            n += 1;
            n < 3
        })
        .unwrap();
        eng.cancel().unwrap();

        let old_ring = IndexKeyring::single_versioned(old.clone(), OLD_V);
        for (p, pt) in &files {
            assert_eq!(peek_key_version(p).unwrap(), Some(OLD_V));
            assert_eq!(&read_enc(p, &old_ring), pt);
        }
        assert_eq!(keyring.current_version(), OLD_V);
    }

    #[test]
    fn foreign_key_file_at_new_version_wedges_verify_and_complete() {
        // Issue #488 P0.3: a file stamped with the NEW key_version but encrypted
        // under a THIRD (foreign) key — as an interrupted second rotation would
        // leave — must NOT be treated as already-migrated. verify_complete and
        // complete must refuse (ForeignKeyVersionFile), never retire the old key.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0xD0), cipher_from_seed(0xD1));
        let foreign = cipher_from_seed(0xEE); // neither old nor new

        // A normal old-key file plus a file stamped NEW_V but written by the
        // foreign key.
        write_enc(&indexes.join("normal.idx"), &old, OLD_V, b"payload-normal");
        write_enc(
            &indexes.join("foreign.idx"),
            &foreign,
            NEW_V,
            b"payload-foreign",
        );

        let eng = engine(&indexes, &old, &new);
        // Full forward pass: the old-key file re-encrypts fine, but the
        // foreign-key file (already at NEW_V number) is detected on the skip
        // path and wedges.
        let err = eng.re_encrypt(&mut |_| true).unwrap_err();
        assert!(
            matches!(err, RotationError::ForeignKeyVersionFile { .. }),
            "expected ForeignKeyVersionFile, got {err:?}"
        );

        // verify_complete independently refuses (key-identity, not number).
        let verr = eng.verify_complete().unwrap_err();
        assert!(
            matches!(verr, RotationError::ForeignKeyVersionFile { .. }),
            "verify_complete must wedge on a foreign-key new-version file, got {verr:?}"
        );

        // complete() must therefore refuse and NOT retire the old generation.
        assert!(eng.complete().is_err());
        assert!(
            eng.status().is_ok(),
            "engine still holds both generations after refusal"
        );
    }

    #[test]
    fn plaintext_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0x01), cipher_from_seed(0x02));
        seed_old_files(&indexes, &old);
        // A legacy plaintext file (no AEIX header) is not enumerated/rotated.
        let plain = indexes.join("legacy.idx");
        std::fs::write(&plain, b"\x00plaintext-not-encrypted").unwrap();

        let eng = engine(&indexes, &old, &new);
        let status_before = eng.status().unwrap();
        assert_eq!(
            status_before.plaintext, 0,
            "plaintext file is not enumerated"
        );
        eng.re_encrypt(&mut |_| true).unwrap();
        // The plaintext file is untouched.
        assert_eq!(
            std::fs::read(&plain).unwrap(),
            b"\x00plaintext-not-encrypted"
        );
    }

    // ── Issue #3618: manifest-/index-body decrypt probe ──────────────

    #[test]
    fn verify_decryptable_passes_with_correct_key() {
        // Files written and probed with the SAME key: every probed body
        // authenticates, nothing fails.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let good = cipher_from_seed(0x11);

        write_enc(
            &indexes.join("manifest.idx"),
            &good,
            OLD_V,
            b"MANIFEST-body-0",
        );
        write_enc(
            &indexes.join("graph").join("adjacency.idx"),
            &good,
            OLD_V,
            b"graph-body-1",
        );

        let ring = IndexKeyring::single_versioned(good.clone(), OLD_V);
        let eng = IndexKeyRotation::new(&indexes, ring, OLD_V, good.clone(), OLD_V, good.clone());

        let report = eng.verify_decryptable().unwrap();
        assert!(
            report.decrypt_failed.is_empty(),
            "correct key must decrypt every probed body: {report:?}"
        );
        assert!(
            report.decrypted_ok > 0,
            "at least one body must be probed and decrypt: {report:?}"
        );
        assert!(report.all_decrypted());
    }

    #[test]
    fn verify_decryptable_catches_same_version_wrong_key() {
        // THE core red test (Issue #3618): a wrong key that shares the SAME
        // key_version number passes header classification (status()) but MUST be
        // caught by the body decrypt probe.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let good = cipher_from_seed(0x11); // key material X
        let wrong = cipher_from_seed(0x99); // DIFFERENT key material Y

        // Bodies written by the GOOD key at key_version 1.
        write_enc(
            &indexes.join("manifest.idx"),
            &good,
            OLD_V,
            b"MANIFEST-body",
        );
        write_enc(
            &indexes.join("graph").join("adjacency.idx"),
            &good,
            OLD_V,
            b"graph-body",
        );

        // Engine holds the WRONG key at the SAME version 1.
        let wrong_ring = IndexKeyring::single_versioned(wrong.clone(), OLD_V);
        let eng = IndexKeyRotation::new(
            &indexes,
            wrong_ring,
            OLD_V,
            wrong.clone(),
            OLD_V,
            wrong.clone(),
        );

        // (a) Header classification PASSES — this is the bug: the version number
        // matches, so status() sees only "current" files and nothing unknown.
        let status = eng.status().unwrap();
        assert!(
            status.at_current > 0,
            "header classification should see current-version files: {status:?}"
        );
        assert_eq!(
            status.unknown, 0,
            "header classification cannot detect the wrong key: {status:?}"
        );

        // (b) The body decrypt probe CATCHES it: nothing authenticates under the
        // wrong key.
        let report = eng.verify_decryptable().unwrap();
        assert!(
            !report.decrypt_failed.is_empty(),
            "decrypt probe must flag the wrong key: {report:?}"
        );
        assert_eq!(
            report.decrypted_ok, 0,
            "no body can decrypt under the wrong key: {report:?}"
        );
        assert!(!report.all_decrypted());
    }

    #[test]
    fn verify_decryptable_ignores_plaintext_files() {
        // A legacy plaintext (non-AEIX) file is counted as plaintext, never as a
        // decrypt failure.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let good = cipher_from_seed(0x11);

        write_enc(
            &indexes.join("manifest.idx"),
            &good,
            OLD_V,
            b"MANIFEST-body",
        );
        std::fs::create_dir_all(&indexes).unwrap();
        std::fs::write(indexes.join("legacy.idx"), b"\x00plaintext-not-encrypted").unwrap();

        let ring = IndexKeyring::single_versioned(good.clone(), OLD_V);
        let eng = IndexKeyRotation::new(&indexes, ring, OLD_V, good.clone(), OLD_V, good.clone());

        let report = eng.verify_decryptable().unwrap();
        assert_eq!(
            report.plaintext, 1,
            "the plaintext file must be counted as plaintext: {report:?}"
        );
        assert!(
            report.decrypt_failed.is_empty(),
            "a plaintext file is not a decrypt failure: {report:?}"
        );
        assert!(
            report.decrypted_ok >= 1,
            "the encrypted manifest must still be probed and decrypt: {report:?}"
        );
    }

    #[test]
    fn verify_decryptable_catches_wrong_key_in_one_of_multiple_generations() {
        // Multi-generation coverage: bodies exist at BOTH key_version 1 and 2.
        // The keyring holds the CORRECT v1 cipher but a WRONG v2 cipher (same
        // version slot, different key bytes). The per-key-version dedup probes one
        // representative body per generation, so the v1 files decrypt while the v2
        // representative fails — exactly what would happen if a later generation's
        // configured key were wrong.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let good_v1 = cipher_from_seed(0x11); // correct v1 key material
        let good_v2 = cipher_from_seed(0x22); // the key that actually wrote v2 files
        let wrong_v2 = cipher_from_seed(0x99); // different bytes, same version slot

        // v1 bodies (manifest + one file) written by the good v1 key.
        write_enc(
            &indexes.join("manifest.idx"),
            &good_v1,
            OLD_V,
            b"MANIFEST-v1",
        );
        write_enc(
            &indexes.join("graph").join("adjacency.idx"),
            &good_v1,
            OLD_V,
            b"graph-v1",
        );
        // v2 bodies written by the good v2 key (a later generation on disk).
        write_enc(
            &indexes.join("vector").join("emb").join("meta.idx"),
            &good_v2,
            NEW_V,
            b"vector-v2",
        );

        // Keyring: correct v1, WRONG v2.
        let ring = IndexKeyring::single_versioned(good_v1.clone(), OLD_V);
        ring.add_generation(NEW_V, wrong_v2.clone());
        let eng = IndexKeyRotation::new(
            &indexes,
            ring,
            OLD_V,
            good_v1.clone(),
            NEW_V,
            wrong_v2.clone(),
        );

        let report = eng.verify_decryptable().unwrap();
        assert!(
            !report.decrypt_failed.is_empty(),
            "the v2 representative under the wrong key must fail: {report:?}"
        );
        assert!(
            report.decrypted_ok >= 1,
            "the v1 files must still decrypt under the correct v1 key: {report:?}"
        );
    }

    #[test]
    fn verify_decryptable_always_probes_manifest() {
        // Bounded-cost design: the manifest is ALWAYS probed, plus the first file
        // (sorted) of each distinct key_version. With three same-version files —
        // one sorting before `manifest.idx`, the manifest, one sorting after —
        // the probe hits exactly two: `aaa.idx` (first-of-version) and the
        // always-probed manifest; `zzz.idx` (a third same-version body) is
        // intentionally NOT probed.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let good = cipher_from_seed(0x11);

        write_enc(&indexes.join("aaa.idx"), &good, OLD_V, b"aaa-body");
        write_enc(
            &indexes.join("manifest.idx"),
            &good,
            OLD_V,
            b"MANIFEST-body",
        );
        write_enc(&indexes.join("zzz.idx"), &good, OLD_V, b"zzz-body");

        let ring = IndexKeyring::single_versioned(good.clone(), OLD_V);
        let eng = IndexKeyRotation::new(&indexes, ring, OLD_V, good.clone(), OLD_V, good.clone());

        let report = eng.verify_decryptable().unwrap();
        assert_eq!(
            report.probed, 2,
            "expected manifest + aaa.idx probed, zzz.idx skipped: {report:?}"
        );
        assert!(
            report.all_decrypted(),
            "all probed bodies decrypt: {report:?}"
        );
    }

    #[test]
    fn verify_decryptable_empty_or_missing_manifest_is_pass() {
        let good = cipher_from_seed(0x11);

        // (a) Non-existent indexes dir: the exists() guard means nothing is
        // walked; the probe returns a clean pass (no panic).
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let ring = IndexKeyring::single_versioned(good.clone(), OLD_V);
        let eng = IndexKeyRotation::new(&missing, ring, OLD_V, good.clone(), OLD_V, good.clone());
        let report = eng.verify_decryptable().unwrap();
        assert_eq!(
            report.probed, 0,
            "nothing to probe in a missing dir: {report:?}"
        );
        assert!(report.decrypt_failed.is_empty());
        assert!(report.all_decrypted());

        // (b) Encrypted non-manifest files but NO manifest.idx: still a pass — the
        // first-of-version representative is probed and decrypts.
        let dir2 = tempfile::tempdir().unwrap();
        let indexes = dir2.path().join("indexes");
        write_enc(
            &indexes.join("graph").join("adjacency.idx"),
            &good,
            OLD_V,
            b"graph-body",
        );
        let ring2 = IndexKeyring::single_versioned(good.clone(), OLD_V);
        let eng2 = IndexKeyRotation::new(&indexes, ring2, OLD_V, good.clone(), OLD_V, good.clone());
        let report2 = eng2.verify_decryptable().unwrap();
        assert!(
            report2.decrypt_failed.is_empty(),
            "no manifest present is not a failure: {report2:?}"
        );
        assert!(report2.all_decrypted());
    }

    #[test]
    fn nonce_uniqueness_under_new_key() {
        // Re-encrypting many files under the new key must produce distinct
        // nonces (fresh per call); no nonce is copied from the old ciphertext.
        let dir = tempfile::tempdir().unwrap();
        let indexes = dir.path().join("indexes");
        let (old, new) = (cipher_from_seed(0xC0), cipher_from_seed(0xC1));
        // Many files with identical plaintext (worst case for nonce reuse).
        let mut paths = Vec::new();
        for i in 0..25 {
            let p = indexes.join(format!("f{i}.idx"));
            write_enc(&p, &old, OLD_V, b"identical-plaintext");
            paths.push(p);
        }
        let eng = engine(&indexes, &old, &new);
        eng.re_encrypt(&mut |_| true).unwrap();

        // AES-256-GCM wire format after the 10-byte header is [nonce(12)]...
        use std::collections::HashSet;
        let mut nonces = HashSet::new();
        for p in &paths {
            let raw = std::fs::read(p).unwrap();
            let nonce = raw[ENC_HEADER_LEN..ENC_HEADER_LEN + 12].to_vec();
            assert!(
                nonces.insert(nonce),
                "duplicate nonce under new key for {p:?}"
            );
        }
        assert_eq!(nonces.len(), paths.len());
    }
}
