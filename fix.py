import sys
content = open("src/encryption/key_provider.rs").read()
old_get_mek = """    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let content = std::fs::read(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                KeyProviderError::KeyNotFound
            } else {
                KeyProviderError::Io(e)
            }
        })?;
        Self::parse_key(&content)
    }"""
new_get_mek = """    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        use std::io::Read;
        let file = std::fs::File::open(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                KeyProviderError::KeyNotFound
            } else {
                KeyProviderError::Io(e)
            }
        })?;

        // Protect against OOM: limit read size since valid keys are at most ~64 bytes
        let mut content = Vec::with_capacity(64);
        file.take(128).read_to_end(&mut content).map_err(KeyProviderError::Io)?;

        Self::parse_key(&content)
    }"""
content = content.replace(old_get_mek, new_get_mek)
test_target = """    #[test]
    fn provider_name() {"""
new_test = """    #[test]
    fn file_provider_large_file_oom_protection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large_key.bin");

        let file = std::fs::File::create(&path).unwrap();
        file.set_len(1024 * 1024).unwrap();

        let provider = FileKeyProvider::new(&path);
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::InvalidKeyFormat(_)),
            "expected InvalidKeyFormat, got: {err}"
        );
    }

    #[test]
    fn provider_name() {"""
content = content.replace(test_target, new_test)
open("src/encryption/key_provider.rs", "w").write(content)
