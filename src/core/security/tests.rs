use super::*;
use argon2::Argon2;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit},
};
use rand::RngCore;
use std::io::Write;
use tempfile::NamedTempFile;
use serial_test::serial;

#[test]
fn test_master_key_properties() {
    let key_data = [1u8; 32];
    let version = 42;
    let master_key = MasterKey::new(key_data, version);

    assert_eq!(master_key.version(), version);
    assert_eq!(master_key.as_bytes(), &key_data);
}

struct MockProvider;

#[async_trait]
impl KeyProvider for MockProvider {
    async fn get_master_key(&self) -> Result<MasterKey, KeyError> {
        Ok(MasterKey::new([0u8; 32], 1))
    }

    async fn get_key_version(&self, _version: u32) -> Result<MasterKey, KeyError> {
        Ok(MasterKey::new([0u8; 32], 1))
    }

    fn current_version(&self) -> u32 {
        1
    }

    fn name(&self) -> &str {
        "Mock"
    }
}

#[tokio::test]
async fn test_provider_trait_object() {
    let provider = MockProvider;
    let key = provider.get_master_key().await.unwrap();
    assert_eq!(key.version(), 1);
}

// Helper to create an encrypted key file matching the expected format
fn create_test_key_file(passphrase: &str, master_key_bytes: &[u8; 32]) -> NamedTempFile {
    let mut file = NamedTempFile::new().unwrap();

    // 1. Generate Salt (16 bytes)
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);

    // 2. Derive KEK using Argon2id
    let mut kek = [0u8; 32];
    // Must match implementation params: 64MB, 3 iters, 4 threads
    let params = argon2::Params::new(64 * 1024, 3, 4, Some(32)).unwrap();
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    argon2.hash_password_into(
        passphrase.as_bytes(),
        &salt,
        &mut kek
    ).unwrap();

    // 3. Encrypt
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&kek));
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher.encrypt(nonce, master_key_bytes.as_slice()).unwrap();

    // 4. Write to file: Salt || Nonce || Ciphertext
    file.write_all(&salt).unwrap();
    file.write_all(&nonce_bytes).unwrap();
    file.write_all(&ciphertext).unwrap();

    file
}

#[tokio::test]
async fn test_file_provider() {
    let passphrase = "correct-horse-battery-staple";
    let master_key_data = [42u8; 32];
    let key_file = create_test_key_file(passphrase, &master_key_data);

    let provider = FileKeyProvider::new(key_file.path(), passphrase);

    let key = provider.get_master_key().await.expect("Should load key");
    assert_eq!(key.as_bytes(), &master_key_data);

    // Test get_key_version
    let key_v1 = provider.get_key_version(1).await.expect("Should load version 1");
    assert_eq!(key_v1.as_bytes(), &master_key_data);

    // Test get_key_version mismatch
    match provider.get_key_version(99).await {
        Err(KeyError::NotFound) => {},
        _ => panic!("Expected NotFound for version 99"),
    }
}

#[tokio::test]
async fn test_file_provider_errors() {
    let passphrase = "correct-horse-battery-staple";
    let master_key_data = [42u8; 32];

    // 1. File Not Found
    let provider = FileKeyProvider::new("non_existent_file.key", passphrase);
    match provider.get_master_key().await {
        Err(KeyError::NotFound) => {},
        _ => panic!("Expected NotFound for missing file"),
    }

    // 2. Wrong Passphrase
    let key_file = create_test_key_file(passphrase, &master_key_data);
    let provider = FileKeyProvider::new(key_file.path(), "wrong-passphrase");
    match provider.get_master_key().await {
        Err(KeyError::DecryptionFailed) => {},
        _ => panic!("Expected DecryptionFailed for wrong passphrase"),
    }

    // 3. Corrupted File (Too Short)
    let mut short_file = NamedTempFile::new().unwrap();
    short_file.write_all(&[0u8; 10]).unwrap();
    let provider = FileKeyProvider::new(short_file.path(), passphrase);
    match provider.get_master_key().await {
        Err(KeyError::ConfigError(msg)) if msg.contains("too short") => {},
        _ => panic!("Expected ConfigError for short file"),
    }
}

#[tokio::test]
#[serial]
async fn test_env_provider() {
    let var_name = "GALLIFREYDB_TEST_KEY_12345";
    let master_key_data = [33u8; 32];
    let encoded = BASE64.encode(master_key_data);

    // Safety: modifying env vars in tests is tricky with threads.
    // We use a unique name and unsafe block as required by recent Rust versions.
    unsafe {
        std::env::set_var(var_name, encoded);
    }

    let provider = EnvKeyProvider::new(var_name);
    let result = provider.get_master_key().await;

    let key = result.expect("Should load key from env");
    assert_eq!(key.as_bytes(), &master_key_data);

    // Test get_key_version
    let key_v1 = provider.get_key_version(1).await.expect("Should load version 1");
    assert_eq!(key_v1.as_bytes(), &master_key_data);

    // Test get_key_version mismatch
    match provider.get_key_version(99).await {
        Err(KeyError::NotFound) => {},
        _ => panic!("Expected NotFound for version 99"),
    }

    // Clean up
    unsafe {
        std::env::remove_var(var_name);
    }
}

#[tokio::test]
#[serial]
async fn test_env_provider_errors() {
    let var_name = "GALLIFREYDB_TEST_KEY_ERROR";
    let provider = EnvKeyProvider::new(var_name);

    // 1. Missing Env Var
    unsafe { std::env::remove_var(var_name); }
    match provider.get_master_key().await {
        Err(KeyError::NotFound) => {},
        _ => panic!("Expected NotFound for missing env var"),
    }

    // 2. Invalid Base64
    unsafe { std::env::set_var(var_name, "not-base-64!!!"); }
    match provider.get_master_key().await {
        Err(KeyError::ConfigError(msg)) if msg.contains("Invalid base64") => {},
        _ => panic!("Expected ConfigError for invalid base64"),
    }

    // 3. Invalid Length
    let short_key = [0u8; 10];
    let encoded = BASE64.encode(short_key);
    unsafe { std::env::set_var(var_name, encoded); }
    match provider.get_master_key().await {
        Err(KeyError::ConfigError(msg)) if msg.contains("Invalid key length") => {},
        _ => panic!("Expected ConfigError for invalid key length"),
    }

    // Cleanup
    unsafe { std::env::remove_var(var_name); }
}

#[cfg(feature = "aws-kms")]
#[tokio::test]
async fn test_kms_provider() {
    use super::kms::{AwsKmsProvider, KmsOperations};

    struct MockKmsOps {
        mock_plaintext: [u8; 32],
    }

    #[async_trait]
    impl KmsOperations for MockKmsOps {
        async fn decrypt(&self, _ciphertext: &[u8]) -> Result<Vec<u8>, String> {
            Ok(self.mock_plaintext.to_vec())
        }

        async fn generate_data_key(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
            // Return dummy key and dummy ciphertext
            Ok((self.mock_plaintext.to_vec(), vec![1, 2, 3, 4]))
        }
    }

    let mock_key = [77u8; 32];
    let ops = MockKmsOps {
        mock_plaintext: mock_key,
    };

    let temp_file = NamedTempFile::new().unwrap();
    let path = temp_file.path().to_path_buf();
    // Delete file so it triggers "generate_new" logic
    temp_file.close().unwrap();

    let provider = AwsKmsProvider::new_with_ops(Box::new(ops), path.clone());

    // 1. Should generate new key
    let key = provider
        .get_master_key()
        .await
        .expect("Should generate key");
    assert_eq!(key.as_bytes(), &mock_key);
    assert!(path.exists());

    // 2. Should load existing key (simulate restart)
    let ops2 = MockKmsOps {
        mock_plaintext: mock_key,
    };
    let provider2 = AwsKmsProvider::new_with_ops(Box::new(ops2), path.clone());

    let key2 = provider2.get_master_key().await.expect("Should load key");
    assert_eq!(key2.as_bytes(), &mock_key);
}

#[cfg(feature = "vault")]
#[tokio::test]
async fn test_vault_provider() {
    use super::vault::{VaultOperations, VaultProvider};

    struct MockVaultOps {
        mock_key_base64: String,
    }

    #[async_trait]
    impl VaultOperations for MockVaultOps {
        async fn read_key(&self, mount: &str, path: &str) -> Result<String, String> {
            if mount == "secret" && path == "app/key" {
                Ok(self.mock_key_base64.clone())
            } else {
                Err("Secret not found".to_string())
            }
        }
    }

    let master_key_data = [55u8; 32];
    let encoded = BASE64.encode(master_key_data);

    let ops = MockVaultOps {
        mock_key_base64: encoded,
    };
    let provider = VaultProvider::new_with_ops(Box::new(ops), "secret/app/key").unwrap();

    let key = provider
        .get_master_key()
        .await
        .expect("Should load key from vault");
    assert_eq!(key.as_bytes(), &master_key_data);

    // Test invalid path
    let ops2 = MockVaultOps {
        mock_key_base64: "dummy".to_string(),
    };
    assert!(VaultProvider::new_with_ops(Box::new(ops2), "invalidpath").is_err());
}
