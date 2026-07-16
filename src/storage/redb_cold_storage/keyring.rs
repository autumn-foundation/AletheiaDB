//! Cold-tier (redb) key ring and self-describing value wrapper (Issue #3617 PR3).
//!
//! A full-MEK key rotation re-keys the cold tier by a **transactional bulk
//! re-encrypt** of every stored value: each `node_versions`/`edge_versions`
//! value is decrypted under the OLD cold DEK and re-encrypted under the NEW cold
//! DEK. To read correctly during (and after) such a rotation the cold store must
//! hold BOTH generations at once and select per value, exactly like the WAL
//! ([`WalKeyring`](crate::encryption::wal_encryption::WalKeyring)) and index
//! ([`IndexKeyring`](crate::storage::index_persistence::common::IndexKeyring))
//! keyrings. [`ColdKeyring`] is that holder, deliberately mirroring their shape
//! (a small `Vec` of generations, a `current` stamped into freshly written
//! values, and a single-cipher `match_any` back-compat mode).
//!
//! # Self-describing value wrapper (`ACV1`)
//!
//! Cold values are compress-then-encrypt with empty AAD, and a legacy value is
//! opaque AEAD ciphertext whose leading bytes are effectively random — so a bare
//! 1-byte discriminator would be ambiguous. Re-encrypted values are therefore
//! prefixed with an 8-byte self-describing header:
//!
//! ```text
//! Wrapped value:  [magic "ACV1":4][key_version:u32 LE:4][ AEAD(compress(record)) ]
//! Legacy value:   [ AEAD(compress(record)) ]            # exactly the pre-#3617 format
//! ```
//!
//! Read dispatch ([`parse_cold_wrapper`] in front of decrypt): a value is
//! treated as wrapped ONLY IF it begins with `ACV1` **and** the following `u32`
//! names a generation the live keyring holds; otherwise it is treated as a
//! legacy bare ciphertext and decrypted under the oldest (pre-rotation) cold
//! generation. This double check makes a false positive astronomically
//! unlikely: a genuine legacy ciphertext would have to begin with the exact
//! 4-byte magic (~2⁻³²) AND have its next 4 bytes equal a small, currently-held
//! key-version (~2⁻³² for the tiny set of live versions) — a combined ~2⁻⁶⁴
//! event, and even then it fails LOUDLY as an AEAD authentication error (the
//! wrong slice is fed to `decrypt`), never as silent wrong data. The same
//! 4-byte-magic collision argument is used one layer down by
//! [`COLD_RECORD_MAGIC_V2`](super) for the in-record provenance tag.
//!
//! No key material is ever written into the wrapper (a `key_version` integer
//! only), logged, or rendered by `Debug`.

use std::sync::{Arc, RwLock};

use crate::encryption::Cipher;

/// Magic prefix identifying an `ACV1`-wrapped cold value. Deliberately the
/// printable ASCII `b"ACV1"` (**A**letheia **C**old **V**alue v**1**), distinct
/// from the record-level [`COLD_RECORD_MAGIC_V2`](super::COLD_RECORD_MAGIC_V2)
/// `[0xA1,0x37,0xC0,0xDE]` / [`COLD_RECORD_MAGIC_V3`](super::COLD_RECORD_MAGIC_V3)
/// `[0xB2,0x48,0xD1,0xEF]` — those tag the *record* inside the ciphertext, this
/// tags the *stored value* outside it, so they never occupy the same byte
/// position, but keeping the values disjoint avoids any reviewer confusion.
pub(super) const COLD_VALUE_MAGIC: [u8; 4] = *b"ACV1";

/// Total length of the `ACV1` wrapper prefix (4-byte magic + 4-byte key_version).
pub(super) const COLD_VALUE_WRAPPER_LEN: usize = 8;

/// The `key_version` a fresh (never-rotated) encrypted cold store stamps into
/// its `ACV1` value wrappers. Mirrors [`INITIAL_WAL_KEY_VERSION`] /
/// `ENC_INDEX_KEY_VERSION_V1` so all three layers number their first generation
/// identically.
///
/// [`INITIAL_WAL_KEY_VERSION`]: crate::encryption::wal_encryption::INITIAL_WAL_KEY_VERSION
pub(super) const INITIAL_COLD_KEY_VERSION: u32 = 1;

/// Prepend the `ACV1` wrapper to an AEAD ciphertext, stamping `key_version`.
pub(super) fn wrap_cold_value(key_version: u32, ciphertext: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(COLD_VALUE_WRAPPER_LEN + ciphertext.len());
    out.extend_from_slice(&COLD_VALUE_MAGIC);
    out.extend_from_slice(&key_version.to_le_bytes());
    out.extend_from_slice(ciphertext);
    out
}

/// If `value` begins with the `ACV1` magic and is long enough to carry the
/// key-version, return `(key_version, ciphertext_without_wrapper)`; otherwise
/// `None` (a legacy, unwrapped value). This does NOT consult the keyring — the
/// caller checks whether the returned `key_version` names a held generation and
/// falls back to the legacy path if not.
pub(super) fn parse_cold_wrapper(value: &[u8]) -> Option<(u32, &[u8])> {
    if value.len() < COLD_VALUE_WRAPPER_LEN || value[..4] != COLD_VALUE_MAGIC {
        return None;
    }
    let key_version = u32::from_le_bytes([value[4], value[5], value[6], value[7]]);
    Some((key_version, &value[COLD_VALUE_WRAPPER_LEN..]))
}

#[derive(Clone)]
struct ColdKeyGeneration {
    key_version: u32,
    cipher: Arc<dyn Cipher>,
}

struct ColdKeyringInner {
    /// All live generations (1 before rotation, 2 during, 1 after). Small.
    generations: Vec<ColdKeyGeneration>,
    /// Version stamped into freshly written values (the newest generation).
    current_version: u32,
    /// Back-compat mode: a lone generation created from a single cipher decrypts
    /// ANY value (any `key_version`, or a legacy value carrying no wrapper) with
    /// that one cipher. This is the never-rotated steady state and, crucially,
    /// the post-provider-switch state: after a completed rotation the operator
    /// reopens under the new key alone, so the single new-DEK keyring must still
    /// read the new-DEK values regardless of the `key_version` they were stamped
    /// with during the rotation.
    match_any: bool,
}

/// A cheaply-cloneable, shared-mutable set of cold DEK ciphers addressed by
/// `key_version` (Issue #3617 PR3).
///
/// Clones share the same underlying state, so the rotation driver can advance
/// the current generation (making new writes stamp + encrypt under the new DEK)
/// while readers holding clones observe the change immediately. Never exposes
/// key material; `Debug` is redacted.
#[derive(Clone)]
pub struct ColdKeyring {
    inner: Arc<RwLock<ColdKeyringInner>>,
}

impl std::fmt::Debug for ColdKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material — only opaque generation metadata.
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("ColdKeyring")
            .field("generations", &inner.generations.len())
            .field("current_version", &inner.current_version)
            .field("match_any", &inner.match_any)
            .finish()
    }
}

impl ColdKeyring {
    /// A single-generation keyring from one cipher (the non-rotation path).
    ///
    /// Reads decrypt any value (any wrapper `key_version`, or a legacy value with
    /// no wrapper) with this cipher and writes stamp
    /// [`INITIAL_COLD_KEY_VERSION`].
    pub fn single(cipher: Arc<dyn Cipher>) -> Self {
        Self::single_versioned(cipher, INITIAL_COLD_KEY_VERSION)
    }

    /// A single-generation cold keyring pinned to an explicit `key_version`
    /// (Issue #488 version-provisioning parity). Reads decrypt every value with
    /// this one cipher (`match_any`, byte-identical to [`Self::single`]); only
    /// the write-stamp / reported [`current_version`](Self::current_version) is
    /// pinned to `key_version`. The durable `open()` path builds the cold keyring
    /// at the provisioned key version so freshly written values stamp the real
    /// version instead of a stale [`INITIAL_COLD_KEY_VERSION`], keeping the cold
    /// layer in lockstep with index/WAL after a rotate-then-reopen.
    pub fn single_versioned(cipher: Arc<dyn Cipher>, key_version: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(ColdKeyringInner {
                generations: vec![ColdKeyGeneration {
                    key_version,
                    cipher,
                }],
                current_version: key_version,
                match_any: true,
            })),
        }
    }

    /// The current (write) cipher and the `key_version` it stamps into new value
    /// wrappers, or `None` if the keyring somehow holds no generation.
    pub fn current(&self) -> Option<(Arc<dyn Cipher>, u32)> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let v = inner.current_version;
        inner
            .generations
            .iter()
            .find(|g| g.key_version == v)
            .map(|g| (g.cipher.clone(), v))
    }

    /// The version freshly written values are stamped with.
    pub fn current_version(&self) -> u32 {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current_version
    }

    /// Resolve the decryption cipher for a value, given the `key_version` read
    /// from its wrapper (`Some` for an `ACV1`-wrapped value, `None` for a legacy
    /// value that carries no wrapper).
    ///
    /// * `match_any` (single-cipher) keyring → the sole cipher for every value
    ///   (never-rotated / post-switch steady state).
    /// * strict keyring, legacy value (`None`) → the OLDEST (minimum-version,
    ///   i.e. pre-rotation) generation — the DEK legacy values were written under
    ///   before the rotation advanced the generation.
    /// * strict keyring, `Some(kv)` → the generation stamped `kv`, or `None` when
    ///   no such generation is held (a genuine wrong/absent key surfaces as a loud
    ///   AEAD failure downstream rather than silent wrong data).
    pub(super) fn cipher_for_value(&self, key_version: Option<u32>) -> Option<Arc<dyn Cipher>> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        if inner.match_any {
            return inner.generations.first().map(|g| g.cipher.clone());
        }
        match key_version {
            Some(kv) => inner
                .generations
                .iter()
                .find(|g| g.key_version == kv)
                .map(|g| g.cipher.clone()),
            None => inner
                .generations
                .iter()
                .min_by_key(|g| g.key_version)
                .map(|g| g.cipher.clone()),
        }
    }

    /// Whether a generation with `key_version` is currently held (a `match_any`
    /// single keyring holds every version).
    pub(super) fn has_version(&self, key_version: u32) -> bool {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.match_any
            || inner
                .generations
                .iter()
                .any(|g| g.key_version == key_version)
    }

    /// Add a new generation and make it current. Switches the keyring to strict
    /// per-version dispatch (leaving `match_any`). Idempotent for a version
    /// already present (its cipher is replaced). Used by the rotation driver to
    /// install the new cold DEK before the bulk re-encrypt pass.
    pub(super) fn add_generation(&self, key_version: u32, cipher: Arc<dyn Cipher>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.match_any = false;
        inner.generations.retain(|g| g.key_version != key_version);
        inner.generations.push(ColdKeyGeneration {
            key_version,
            cipher,
        });
        inner.current_version = key_version;
    }

    /// Retire every generation except `key_version`, which becomes the sole,
    /// current generation. Used by the rotation driver at completion once every
    /// value is re-wrapped under the new generation, so the old cold DEK can be
    /// dropped.
    pub(super) fn retain_only(&self, key_version: u32) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.generations.retain(|g| g.key_version == key_version);
        inner.current_version = key_version;
        inner.match_any = false;
    }
}
