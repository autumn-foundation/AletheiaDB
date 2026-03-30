//! Key derivation via HKDF-SHA256.
//!
//! Derives per-component Data Encryption Keys (DEKs) from the Master Encryption
//! Key (MEK) using HKDF-SHA256. Each storage component (WAL, indexes, cold
//! storage, checkpoints) gets a unique DEK to limit blast radius if any single
//! key is compromised.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::encryption::KeyDerivationError;

/// HKDF info-string context for WAL encryption keys.
pub const WAL_DEK_CONTEXT: &str = "wal";

/// HKDF info-string context for index encryption keys.
pub const INDEX_DEK_CONTEXT: &str = "index";

/// HKDF info-string context for cold storage encryption keys.
pub const COLD_DEK_CONTEXT: &str = "cold";

/// HKDF info-string context for checkpoint encryption keys.
pub const CHECKPOINT_DEK_CONTEXT: &str = "checkpoint";

/// Derives per-component DEKs from a master encryption key using HKDF-SHA256.
///
/// The info string format is `aletheiadb-{component}-dek-v1` which binds each
/// derived key to a specific component and allows future key-schedule versioning.
pub struct KeyDerivation {
    mek: Zeroizing<[u8; 32]>,
}

impl KeyDerivation {
    /// Create a new key derivation context from a master encryption key.
    pub fn new(mek: Zeroizing<[u8; 32]>) -> Self {
        Self { mek }
    }

    /// Derive a 32-byte DEK for the given component name.
    ///
    /// The component name is embedded in the HKDF info string as
    /// `aletheiadb-{component}-dek-v1`.
    pub fn derive_dek(&self, component: &str) -> Result<Zeroizing<[u8; 32]>, KeyDerivationError> {
        let hkdf = Hkdf::<Sha256>::new(None, self.mek.as_ref());
        let info = format!("aletheiadb-{component}-dek-v1");

        let mut okm = Zeroizing::new([0u8; 32]);
        hkdf.expand(info.as_bytes(), okm.as_mut())
            .map_err(|e| KeyDerivationError::DerivationFailed(e.to_string()))?;

        Ok(okm)
    }

    /// Derive the DEK for WAL encryption.
    pub fn derive_wal_dek(&self) -> Result<Zeroizing<[u8; 32]>, KeyDerivationError> {
        self.derive_dek(WAL_DEK_CONTEXT)
    }

    /// Derive the DEK for index encryption.
    pub fn derive_index_dek(&self) -> Result<Zeroizing<[u8; 32]>, KeyDerivationError> {
        self.derive_dek(INDEX_DEK_CONTEXT)
    }

    /// Derive the DEK for cold storage encryption.
    pub fn derive_cold_dek(&self) -> Result<Zeroizing<[u8; 32]>, KeyDerivationError> {
        self.derive_dek(COLD_DEK_CONTEXT)
    }

    /// Derive the DEK for checkpoint encryption.
    pub fn derive_checkpoint_dek(&self) -> Result<Zeroizing<[u8; 32]>, KeyDerivationError> {
        self.derive_dek(CHECKPOINT_DEK_CONTEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    fn random_mek() -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(key.as_mut());
        key
    }

    #[test]
    fn derive_dek_deterministic() {
        let mek = random_mek();
        let kd = KeyDerivation::new(mek.clone());

        let dek1 = kd.derive_dek("wal").unwrap();
        let dek2 = kd.derive_dek("wal").unwrap();
        assert_eq!(dek1.as_ref(), dek2.as_ref());
    }

    #[test]
    fn different_components_different_deks() {
        let mek = random_mek();
        let kd = KeyDerivation::new(mek);

        let wal = kd.derive_wal_dek().unwrap();
        let index = kd.derive_index_dek().unwrap();
        let cold = kd.derive_cold_dek().unwrap();
        let checkpoint = kd.derive_checkpoint_dek().unwrap();

        // All four must be distinct
        let deks: Vec<&[u8; 32]> = vec![&*wal, &*index, &*cold, &*checkpoint];
        for i in 0..deks.len() {
            for j in (i + 1)..deks.len() {
                assert_ne!(deks[i], deks[j], "DEK {i} and {j} must differ");
            }
        }
    }

    #[test]
    fn different_mek_different_deks() {
        let mek1 = random_mek();
        let mek2 = random_mek();
        let kd1 = KeyDerivation::new(mek1);
        let kd2 = KeyDerivation::new(mek2);

        let dek1 = kd1.derive_wal_dek().unwrap();
        let dek2 = kd2.derive_wal_dek().unwrap();
        assert_ne!(dek1.as_ref(), dek2.as_ref());
    }

    #[test]
    fn custom_component_works() {
        let mek = random_mek();
        let kd = KeyDerivation::new(mek);

        let dek = kd.derive_dek("my-custom-component").unwrap();
        assert_eq!(dek.as_ref().len(), 32);
    }

    #[test]
    fn dek_is_32_bytes() {
        let mek = random_mek();
        let kd = KeyDerivation::new(mek);

        assert_eq!(kd.derive_wal_dek().unwrap().as_ref().len(), 32);
        assert_eq!(kd.derive_index_dek().unwrap().as_ref().len(), 32);
        assert_eq!(kd.derive_cold_dek().unwrap().as_ref().len(), 32);
        assert_eq!(kd.derive_checkpoint_dek().unwrap().as_ref().len(), 32);
    }
}
