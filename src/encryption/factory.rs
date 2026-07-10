//! Cipher factory and algorithm selection.
//!
//! Resolves the `Auto` algorithm choice based on hardware capabilities and
//! constructs the appropriate cipher implementation.

use zeroize::Zeroizing;

use crate::encryption::cipher::{
    AES_256_GCM_ID, Aes256GcmCipher, CHACHA20_POLY1305_ID, ChaCha20Poly1305Cipher, Cipher,
};

/// Supported encryption algorithms.
///
/// `Auto` selects the best algorithm based on hardware capabilities at runtime.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum Algorithm {
    /// AES-256-GCM -- preferred when AES-NI hardware acceleration is available.
    Aes256Gcm,
    /// ChaCha20-Poly1305 -- preferred on platforms without AES-NI.
    ChaCha20Poly1305,
    /// Automatically select the best algorithm based on CPU features.
    #[default]
    Auto,
}

impl Algorithm {
    /// Resolve `Auto` to a concrete algorithm based on hardware capabilities.
    ///
    /// - On x86/x86_64 with AES-NI: resolves to `Aes256Gcm`
    /// - Otherwise: resolves to `ChaCha20Poly1305`
    ///
    /// Non-`Auto` variants pass through unchanged.
    #[must_use]
    pub fn resolve(self) -> Self {
        match self {
            Self::Auto => {
                if cpu_has_aes_ni() {
                    Self::Aes256Gcm
                } else {
                    Self::ChaCha20Poly1305
                }
            }
            concrete => concrete,
        }
    }
}

/// Check whether the CPU supports AES-NI hardware acceleration.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn cpu_has_aes_ni() -> bool {
    is_x86_feature_detected!("aes")
}

/// Non-x86 platforms do not have AES-NI.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn cpu_has_aes_ni() -> bool {
    false
}

/// Create a boxed [`Cipher`] for the given algorithm and key.
///
/// If `algorithm` is [`Algorithm::Auto`], it is resolved first via
/// [`Algorithm::resolve`].
pub fn create_cipher(algorithm: Algorithm, key: &Zeroizing<[u8; 32]>) -> Box<dyn Cipher> {
    match algorithm.resolve() {
        Algorithm::Aes256Gcm => Box::new(Aes256GcmCipher::new(key)),
        Algorithm::ChaCha20Poly1305 => Box::new(ChaCha20Poly1305Cipher::new(key)),
        Algorithm::Auto => unreachable!("resolve() always returns a concrete algorithm"),
    }
}

/// Look up an [`Algorithm`] by its numeric wire-format identifier.
///
/// Returns `None` for unknown IDs (including 0 = plaintext).
pub fn algorithm_from_id(id: u8) -> Option<Algorithm> {
    match id {
        AES_256_GCM_ID => Some(Algorithm::Aes256Gcm),
        CHACHA20_POLY1305_ID => Some(Algorithm::ChaCha20Poly1305),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    fn test_key() -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(key.as_mut());
        key
    }

    #[test]
    fn auto_resolves_to_concrete() {
        let resolved = Algorithm::Auto.resolve();
        assert!(
            resolved == Algorithm::Aes256Gcm || resolved == Algorithm::ChaCha20Poly1305,
            "Auto should resolve to AES or ChaCha, got: {resolved:?}"
        );
        // Must not remain Auto
        assert_ne!(resolved, Algorithm::Auto);
    }

    #[test]
    fn explicit_algorithm_stays_unchanged() {
        assert_eq!(Algorithm::Aes256Gcm.resolve(), Algorithm::Aes256Gcm);
        assert_eq!(
            Algorithm::ChaCha20Poly1305.resolve(),
            Algorithm::ChaCha20Poly1305
        );
    }

    #[test]
    fn create_cipher_roundtrips() {
        let key = test_key();
        let plaintext = b"factory test payload";
        let aad = b"test-aad";

        for algo in [Algorithm::Aes256Gcm, Algorithm::ChaCha20Poly1305] {
            let cipher = create_cipher(algo, &key);
            let encrypted = cipher.encrypt(plaintext, aad).unwrap();
            let decrypted = cipher.decrypt(&encrypted, aad).unwrap();
            assert_eq!(decrypted, plaintext, "roundtrip failed for {algo:?}");
        }
    }

    #[test]
    fn algorithm_from_id_known() {
        assert_eq!(algorithm_from_id(1), Some(Algorithm::Aes256Gcm));
        assert_eq!(algorithm_from_id(2), Some(Algorithm::ChaCha20Poly1305));
    }

    #[test]
    fn algorithm_from_id_unknown() {
        assert_eq!(algorithm_from_id(0), None);
        assert_eq!(algorithm_from_id(255), None);
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(Algorithm::default(), Algorithm::Auto);
    }
}
