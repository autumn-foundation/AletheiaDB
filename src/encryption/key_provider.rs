//! Key provider trait and implementations for sourcing the Master Encryption Key.
//!
//! Providers abstract over where the MEK lives -- a file on disk, an environment
//! variable, or (in the future) a remote KMS. All providers return a
//! `Zeroizing<[u8; 32]>` that is securely erased when dropped.

use std::path::{Path, PathBuf};

use rand::RngCore;
use zeroize::Zeroizing;

use crate::encryption::KeyProviderError;

/// Sources the Master Encryption Key (MEK) for the database.
///
/// Implementations must be `Send + Sync` to allow concurrent access from
/// multiple storage components.
pub trait KeyProvider: Send + Sync {
    /// Retrieve the 32-byte master encryption key.
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError>;

    /// Retrieve a specific key version (Issue #3587).
    ///
    /// The default implementation is for single-version providers (file, env,
    /// passphrase): they hold exactly one key, so any requested version resolves
    /// to that sole key via [`get_mek`](Self::get_mek). Multi-version backends
    /// (e.g. KMS/Vault envelope models) override this to reject unknown versions
    /// with [`KeyProviderError::KeyNotFound`] rather than silently returning the
    /// current key. It never panics.
    fn get_key_version(&self, _version: u32) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        self.get_mek()
    }

    /// Human-readable provider name for logging/diagnostics.
    fn provider_name(&self) -> &str;

    /// Validate that the provider is functional and the key is accessible.
    fn health_check(&self) -> Result<(), KeyProviderError>;
}

/// Supported key file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFormat {
    /// 64-character hex-encoded key (ASCII).
    Hex,
    /// Raw 32-byte binary key.
    Binary,
}

/// Encode bytes as a lowercase hex string.
fn bytes_to_hex(bytes: &[u8]) -> String {
    crate::core::hex::encode(bytes)
}

/// Decode a hex string into bytes. Returns `None` if the string contains
/// non-hex characters or has odd length.
fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    crate::core::hex::decode(hex)
}

/// Reads the MEK from a file on disk.
///
/// Supports both hex-encoded (64 ASCII characters) and raw binary (32 bytes)
/// key formats. Whitespace and trailing newlines are trimmed before parsing.
pub struct FileKeyProvider {
    path: PathBuf,
}

impl FileKeyProvider {
    /// Create a new file-based key provider.
    ///
    /// # Why?
    /// Serves as the primary way for containerized deployments (using mounted secrets) to
    /// pass the master key to the database process.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::key_provider::{FileKeyProvider, KeyProvider};
    ///
    /// let provider = FileKeyProvider::new("/etc/keys/master.key");
    /// assert_eq!(provider.provider_name(), "file");
    /// ```
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Generate a new random key file at the given path, overwriting any
    /// existing file.
    ///
    /// Creates parent directories if they don't exist. The key is written as
    /// 64 hex characters followed by a newline.
    ///
    /// # Security
    /// On Unix the file is created with mode `0600` (owner read/write only)
    /// *before* any key bytes are written, so the master key is **never**
    /// world- or group-readable at any instant -- there is no window between
    /// creation and a subsequent `chmod` during which the real key sits in a
    /// `0644` file. See [`generate_key_file_with_overwrite`] for the
    /// non-overwriting (atomic `O_EXCL`) variant.
    ///
    /// [`generate_key_file_with_overwrite`]: Self::generate_key_file_with_overwrite
    pub fn generate_key_file(path: &Path) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        Self::generate_key_file_with_overwrite(path, true)
    }

    /// Generate a new random key file, refusing to overwrite an existing file
    /// when `overwrite` is `false`.
    ///
    /// When `overwrite` is `false`, the file is created atomically with
    /// `O_EXCL` (`create_new`): if a file already exists at `path` the call
    /// fails (fail-closed) rather than clobbering it, closing the
    /// check-then-write TOCTOU. When `overwrite` is `true`, any existing file
    /// is first removed so the key file is always (re-)created fresh with the
    /// correct mode (a momentary absence exposes no key material).
    ///
    /// # Security
    /// On Unix the file is created with mode `0600` (owner read/write only)
    /// *before* any key bytes are written, so the master key is **never**
    /// world- or group-readable at any instant.
    pub fn generate_key_file_with_overwrite(
        path: &Path,
        overwrite: bool,
    ) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        use std::io::Write;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut key = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(key.as_mut());

        // Keep the hex representation in a zeroizing buffer so the key never
        // lingers in a plain heap `String`.
        let key_hex = Zeroizing::new(format!("{}\n", bytes_to_hex(key.as_ref())));

        if overwrite {
            // Opening an existing file with `truncate` does NOT reset its mode,
            // which could leave a stale (possibly world-readable) file's
            // permissions in place. Remove it first, then re-create fresh with
            // `0600`. A brief absence exposes no key material.
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }

        // Create with `O_EXCL` in both paths (the overwrite path just removed
        // any prior file). This makes creation atomic and guarantees the mode
        // is applied at creation time on Unix -- the key bytes are only ever
        // written into a `0600` file.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(path)?;
        f.write_all(key_hex.as_bytes())?;
        f.flush()?;

        Ok(key)
    }

    /// Parse raw file content into a 32-byte key, auto-detecting format.
    ///
    /// - If the trimmed content is exactly 64 hex characters, decode as hex.
    /// - If the raw content is exactly 32 bytes, treat as binary.
    /// - Otherwise, return an error.
    fn parse_key(content: &[u8]) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        // Try hex first: trim whitespace from the UTF-8 representation
        if let Ok(text) = std::str::from_utf8(content) {
            let trimmed = text.trim();
            if trimmed.len() == 64 {
                let decoded = hex_to_bytes(trimmed).ok_or_else(|| {
                    KeyProviderError::InvalidKeyFormat("64 chars but not valid hex".to_string())
                })?;
                let mut key = Zeroizing::new([0u8; 32]);
                key.copy_from_slice(&decoded);
                return Ok(key);
            }
        }

        // Try raw binary: exactly 32 bytes
        if content.len() == 32 {
            let mut key = Zeroizing::new([0u8; 32]);
            key.copy_from_slice(content);
            return Ok(key);
        }

        Err(KeyProviderError::InvalidKeyFormat(format!(
            "expected 64 hex chars or 32 raw bytes, got {} bytes",
            content.len()
        )))
    }
}

impl KeyProvider for FileKeyProvider {
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let content = std::fs::read(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                KeyProviderError::KeyNotFound
            } else {
                KeyProviderError::Io(e)
            }
        })?;
        Self::parse_key(&content)
    }

    fn provider_name(&self) -> &str {
        "file"
    }

    fn health_check(&self) -> Result<(), KeyProviderError> {
        self.get_mek().map(|_| ())
    }
}

/// Reads the MEK from an environment variable.
///
/// The variable must contain a 64-character hex-encoded key.
pub struct EnvKeyProvider {
    var_name: String,
}

impl EnvKeyProvider {
    /// Create a new environment-variable key provider.
    ///
    /// # Why?
    /// Supports 12-factor application deployments where secrets are injected directly
    /// into the environment space, avoiding the need for ephemeral disk mounts.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::key_provider::{EnvKeyProvider, KeyProvider};
    ///
    /// let provider = EnvKeyProvider::new("MY_MASTER_KEY");
    /// assert_eq!(provider.provider_name(), "env");
    /// ```
    pub fn new(var_name: impl Into<String>) -> Self {
        Self {
            var_name: var_name.into(),
        }
    }
}

impl KeyProvider for EnvKeyProvider {
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let value = std::env::var(&self.var_name).map_err(|_| KeyProviderError::KeyNotFound)?;
        FileKeyProvider::parse_key(value.as_bytes())
    }

    fn provider_name(&self) -> &str {
        "env"
    }

    fn health_check(&self) -> Result<(), KeyProviderError> {
        self.get_mek().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Unique env var counter to avoid test interference
    static ENV_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_env_var(prefix: &str) -> String {
        let id = ENV_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("ALETHEIADB_TEST_{prefix}_{id}_{}", std::process::id())
    }

    fn hex_key_string() -> (String, Zeroizing<[u8; 32]>) {
        let mut key = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(key.as_mut());
        (bytes_to_hex(key.as_ref()), key)
    }

    #[test]
    fn file_provider_hex_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.hex");

        let (hex_str, expected) = hex_key_string();
        std::fs::write(&path, &hex_str).unwrap();

        let provider = FileKeyProvider::new(&path);
        let mek = provider.get_mek().unwrap();
        assert_eq!(mek.as_ref(), expected.as_ref());
    }

    #[test]
    fn file_provider_hex_with_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key_nl.hex");

        let (hex_str, expected) = hex_key_string();
        std::fs::write(&path, format!("{hex_str}\n")).unwrap();

        let provider = FileKeyProvider::new(&path);
        let mek = provider.get_mek().unwrap();
        assert_eq!(mek.as_ref(), expected.as_ref());
    }

    #[test]
    fn file_provider_binary_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key.bin");

        let mut expected = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(expected.as_mut());
        std::fs::write(&path, expected.as_ref()).unwrap();

        let provider = FileKeyProvider::new(&path);
        let mek = provider.get_mek().unwrap();
        assert_eq!(mek.as_ref(), expected.as_ref());
    }

    #[test]
    fn file_provider_invalid_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key_bad.bin");
        std::fs::write(&path, [0u8; 17]).unwrap();

        let provider = FileKeyProvider::new(&path);
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::InvalidKeyFormat(_)),
            "expected InvalidKeyFormat, got: {err}"
        );
    }

    #[test]
    fn file_provider_missing_file() {
        let provider = FileKeyProvider::new("/nonexistent/path/key.hex");
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "expected KeyNotFound, got: {err}"
        );
    }

    #[test]
    fn file_provider_health_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("healthy.hex");

        let (hex_str, _) = hex_key_string();
        std::fs::write(&path, &hex_str).unwrap();

        let provider = FileKeyProvider::new(&path);
        assert!(provider.health_check().is_ok());
    }

    #[test]
    fn file_provider_health_check_missing() {
        let provider = FileKeyProvider::new("/nonexistent/path/key.hex");
        let err = provider.health_check().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "expected KeyNotFound, got: {err}"
        );
    }

    #[test]
    fn generate_key_file_creates_valid_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("generated.hex");

        let generated = FileKeyProvider::generate_key_file(&path).unwrap();

        // Read it back via the provider
        let provider = FileKeyProvider::new(&path);
        let loaded = provider.get_mek().unwrap();
        assert_eq!(generated.as_ref(), loaded.as_ref());
    }

    /// The key file must be `0600` the instant `generate_key_file` returns --
    /// proving the mode is applied at creation time and does NOT depend on any
    /// post-hoc `chmod` by the caller. If it did, this test would observe the
    /// looser default-umask mode.
    #[cfg(unix)]
    #[test]
    fn generate_key_file_is_0600_immediately() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("created_0600.hex");

        let _key = FileKeyProvider::generate_key_file(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "key file must be 0600 at creation time (no post-hoc chmod), got {mode:o}"
        );
    }

    /// The non-overwriting variant must fail (fail-closed) if a file already
    /// exists at the path, and must leave that file's contents untouched.
    #[test]
    fn generate_key_file_no_overwrite_refuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.hex");
        std::fs::write(&path, b"pre-existing contents").unwrap();

        let err = FileKeyProvider::generate_key_file_with_overwrite(&path, false).unwrap_err();
        assert!(
            matches!(err, KeyProviderError::Io(_)),
            "expected an Io (AlreadyExists) error, got: {err}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"pre-existing contents",
            "existing file must be left untouched when overwrite is refused"
        );
    }

    #[test]
    fn env_provider_reads_hex() {
        let var = unique_env_var("HEX");
        let (hex_str, expected) = hex_key_string();
        // SAFETY: test-only, unique var names prevent races
        unsafe { std::env::set_var(&var, &hex_str) };

        let provider = EnvKeyProvider::new(&var);
        let mek = provider.get_mek().unwrap();
        assert_eq!(mek.as_ref(), expected.as_ref());

        // Clean up
        unsafe { std::env::remove_var(&var) };
    }

    #[test]
    fn env_provider_missing_var() {
        let var = unique_env_var("MISSING");
        // Ensure it doesn't exist
        unsafe { std::env::remove_var(&var) };

        let provider = EnvKeyProvider::new(&var);
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "expected KeyNotFound, got: {err}"
        );
    }

    #[test]
    fn provider_name() {
        let file_provider = FileKeyProvider::new("/tmp/key");
        assert_eq!(file_provider.provider_name(), "file");

        let env_provider = EnvKeyProvider::new("MY_KEY");
        assert_eq!(env_provider.provider_name(), "env");
    }

    #[test]
    fn file_provider_get_key_version_returns_sole_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("versioned.hex");

        let (hex_str, expected) = hex_key_string();
        std::fs::write(&path, &hex_str).unwrap();

        let provider = FileKeyProvider::new(&path);
        // A single-version provider resolves ANY requested version to its sole key.
        let v = provider.get_key_version(42).unwrap();
        assert_eq!(v.as_ref(), expected.as_ref());
        assert_eq!(
            provider.get_key_version(0).unwrap().as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn env_provider_get_key_version_returns_sole_key() {
        let var = unique_env_var("VERSIONED");
        let (hex_str, expected) = hex_key_string();
        // SAFETY: test-only, unique var names prevent races
        unsafe { std::env::set_var(&var, &hex_str) };

        let provider = EnvKeyProvider::new(&var);
        let v = provider.get_key_version(7).unwrap();
        assert_eq!(v.as_ref(), expected.as_ref());

        unsafe { std::env::remove_var(&var) };
    }
}
