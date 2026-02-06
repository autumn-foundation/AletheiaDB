# ADR-0026: Encryption-at-Rest Architecture

**Status:** Proposed
**Date:** 2026-01-27
**Deciders:** AletheiaDB Core Team
**Categories:** security, storage, durability, encryption
**Related:** ADR-0007 (WAL Durability), ADR-0023 (Index Persistence), ADR-0025 (Redb Cold Storage)

## Context

AletheiaDB stores sensitive data across multiple persistence layers:
- **WAL segments**: All database mutations including properties and temporal data
- **Index files**: Graph structure, vector embeddings, temporal version chains
- **Cold storage (Redb)**: Compressed historical versions
- **Checkpoint files**: Database state snapshots

Currently, all data is stored in plaintext, creating security risks:

1. **Data Breach Risk**: Unauthorized access to storage media exposes all database contents
2. **Compliance Requirements**: Many industries (healthcare, finance) require encryption-at-rest for regulatory compliance (HIPAA, PCI-DSS, GDPR)
3. **Multi-tenant Isolation**: Shared storage environments require cryptographic isolation
4. **LLM Integration Risk**: Knowledge graphs may contain sensitive business intelligence that must be protected

**Requirements:**

1. Encrypt all persisted data (WAL, indexes, cold storage, checkpoints)
2. Support multiple key management backends (file, env, KMS, HSM)
3. Minimal performance impact on current-state queries (<5% overhead)
4. Maintain ACID guarantees and crash recovery capabilities
5. Enable key rotation without full database re-encryption
6. Provide authenticated encryption (confidentiality + integrity)

## Decision

We implement a **layered encryption-at-rest architecture** with pluggable key providers and per-component Data Encryption Keys (DEKs).

### Architecture Overview

```
                           ┌─────────────────────────────────────────┐
                           │           Key Provider Interface        │
                           │  (File / Env / AWS KMS / GCP / Vault)  │
                           └─────────────────┬───────────────────────┘
                                             │
                                             ▼
                           ┌─────────────────────────────────────────┐
                           │      Master Encryption Key (MEK)        │
                           │         (256-bit, from provider)        │
                           └─────────────────┬───────────────────────┘
                                             │
               ┌─────────────────────────────┼─────────────────────────────┐
               │ HKDF-SHA256                 │                             │
               ▼                             ▼                             ▼
        ┌─────────────┐               ┌─────────────┐               ┌─────────────┐
        │   WAL DEK   │               │  Index DEK  │               │  Cold DEK   │
        │  (256-bit)  │               │  (256-bit)  │               │  (256-bit)  │
        └──────┬──────┘               └──────┬──────┘               └──────┬──────┘
               │                             │                             │
               ▼                             ▼                             ▼
        ┌─────────────┐               ┌─────────────┐               ┌─────────────┐
        │ WAL Segments│               │ Index Files │               │ Redb Tables │
        │ (*.log)     │               │ (*.idx)     │               │ (*.redb)    │
        └─────────────┘               └─────────────┘               └─────────────┘

                                             │
                                             │ HKDF-SHA256
                                             ▼
                                      ┌─────────────┐
                                      │Checkpoint   │
                                      │DEK (256-bit)│
                                      └──────┬──────┘
                                             │
                                             ▼
                                      ┌─────────────┐
                                      │ Checkpoints │
                                      │ (*.ckpt)    │
                                      └─────────────┘
```

### Encryption Algorithm Selection

**Common Cipher Trait:**

```rust
use zeroize::Zeroizing;

/// Common trait for all encryption ciphers
pub trait Cipher: Send + Sync {
    /// Encrypt plaintext with random nonce
    /// Output format: [nonce:12][ciphertext:N][tag:16]
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError>;

    /// Decrypt ciphertext and verify authentication tag
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, EncryptionError>;

    /// Algorithm name for logging/diagnostics
    fn algorithm_name(&self) -> &'static str;
}

/// Key format for file/env providers
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum KeyFormat {
    /// 64 hex characters (256 bits)
    Hex,
    /// 32 raw bytes
    Binary,
}
```

**Primary: AES-256-GCM**

```rust
pub struct Aes256GcmCipher {
    key: Zeroizing<[u8; 32]>,
}

impl Aes256GcmCipher {
    /// Encrypt with random 12-byte nonce
    /// Output format: [nonce:12][ciphertext:N][tag:16]
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError>;

    /// Decrypt and verify authentication tag
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, EncryptionError>;
}
```

**Rationale:**
- Hardware acceleration via AES-NI (available on most modern CPUs)
- NIST-approved, widely audited
- Built-in authentication (GCM mode provides AEAD)
- ~3-5 GB/s throughput on modern hardware with AES-NI

**Fallback: ChaCha20-Poly1305**

```rust
pub struct ChaCha20Poly1305Cipher {
    key: Zeroizing<[u8; 32]>,
}

impl ChaCha20Poly1305Cipher {
    /// Encrypt with random 12-byte nonce
    /// Output format: [nonce:12][ciphertext:N][tag:16]
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError>;

    /// Decrypt and verify authentication tag
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, EncryptionError>;
}
```

**Rationale:**
- Constant-time implementation (resistant to timing attacks)
- No hardware acceleration required (ideal for ARM, embedded, VMs)
- Excellent performance without AES-NI (~1-2 GB/s in software)
- Same security guarantees as AES-256-GCM

**Algorithm Selection Logic:**

```rust
/// Create a cipher from configuration and DEK
/// Note: The key is obtained from the KeyProvider and derived via HKDF,
/// not stored directly in EncryptionConfig.
pub fn select_cipher(
    algorithm: Algorithm,
    dek: Zeroizing<[u8; 32]>,
) -> Box<dyn Cipher> {
    match algorithm {
        Algorithm::Aes256Gcm => Box::new(Aes256GcmCipher::new(dek)),
        Algorithm::ChaCha20Poly1305 => Box::new(ChaCha20Poly1305Cipher::new(dek)),
        Algorithm::Auto => {
            if cpu_has_aes_ni() {
                Box::new(Aes256GcmCipher::new(dek))
            } else {
                Box::new(ChaCha20Poly1305Cipher::new(dek))
            }
        }
    }
}

/// Check for AES-NI hardware support
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn cpu_has_aes_ni() -> bool {
    std::arch::is_x86_feature_detected!("aes")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn cpu_has_aes_ni() -> bool {
    false
}
```

### Key Hierarchy Architecture

**Master Encryption Key (MEK)**

The MEK is a 256-bit key sourced from the configured key provider. It is never used directly for encryption; instead, it derives component-specific DEKs.

**Data Encryption Keys (DEKs)**

DEKs are derived using HKDF-SHA256 with unique context strings:

```rust
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

pub struct KeyDerivation {
    mek: Zeroizing<[u8; 32]>,
}

impl KeyDerivation {
    /// Create from MEK obtained from KeyProvider
    pub fn new(mek: Zeroizing<[u8; 32]>) -> Self {
        Self { mek }
    }

    /// Derive a component-specific DEK
    pub fn derive_dek(&self, component: &str) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(None, self.mek.as_ref());
        let info = format!("aletheiadb-{}-dek-v1", component);
        let mut dek = Zeroizing::new([0u8; 32]);
        hk.expand(info.as_bytes(), dek.as_mut())
            .expect("32 bytes is valid output length");
        dek
    }
}

// DEK derivation contexts
const WAL_DEK_CONTEXT: &str = "wal";       // → "aletheiadb-wal-dek-v1"
const INDEX_DEK_CONTEXT: &str = "index";   // → "aletheiadb-index-dek-v1"
const COLD_DEK_CONTEXT: &str = "cold";     // → "aletheiadb-cold-dek-v1"
const CHECKPOINT_DEK_CONTEXT: &str = "checkpoint"; // → "aletheiadb-checkpoint-dek-v1"
```

**Key Hierarchy Benefits:**
- MEK compromise requires re-keying only the MEK, not all DEKs
- Component isolation: compromising one DEK doesn't expose other components
- Key rotation can be done per-component without full re-encryption

### Key Provider Abstraction

**Trait Definition:**

```rust
/// Trait for key management backends
pub trait KeyProvider: Send + Sync {
    /// Retrieve the Master Encryption Key
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError>;

    /// Provider name for logging/diagnostics
    fn provider_name(&self) -> &str;

    /// Check if the provider is available and properly configured
    fn health_check(&self) -> Result<(), KeyProviderError>;

    /// Rotate the MEK (if supported by the provider)
    fn rotate_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        Err(KeyProviderError::RotationNotSupported)
    }
}

/// Errors from key providers
#[derive(Debug, thiserror::Error)]
pub enum KeyProviderError {
    #[error("Key not found")]
    KeyNotFound,

    #[error("Access denied: {0}")]
    AccessDenied(String),

    #[error("Provider unavailable: {0}")]
    Unavailable(String),

    #[error("Invalid key format: {0}")]
    InvalidKeyFormat(String),

    #[error("Key rotation not supported by this provider")]
    RotationNotSupported,

    #[error("Provider error: {0}")]
    Provider(#[source] Box<dyn std::error::Error + Send + Sync>),
}
```

**Built-in Providers:**

#### 1. File-Based Provider (Development/Testing)

```rust
/// Reads MEK from a file (hex-encoded or raw bytes)
pub struct FileKeyProvider {
    path: PathBuf,
    format: KeyFormat,
}

impl FileKeyProvider {
    pub fn new(path: impl Into<PathBuf>) -> Self;

    /// Generate a new random key file
    pub fn generate_key_file(path: &Path) -> Result<(), KeyProviderError>;
}
```

**File Format Options:**
- `key.hex`: 64 hex characters (256 bits)
- `key.bin`: 32 raw bytes

**Security Note:** File must have restricted permissions (0600). Provider validates permissions on startup.

#### 2. Environment Variable Provider (Containers/CI)

```rust
/// Reads MEK from an environment variable
pub struct EnvKeyProvider {
    var_name: String,
    format: KeyFormat,
}

impl EnvKeyProvider {
    pub fn new(var_name: impl Into<String>) -> Self;
}

// Usage:
// GALLIFREYDB_MEK=<64-hex-chars>
```

**Security Note:** Environment variables may appear in process listings. Use with caution; prefer file or KMS providers for production.

#### 3. AWS KMS Provider

```rust
/// Retrieves MEK from AWS Key Management Service
pub struct AwsKmsProvider {
    client: aws_sdk_kms::Client,
    key_id: String,           // KMS key ARN or alias
    encryption_context: HashMap<String, String>,
}

impl AwsKmsProvider {
    pub async fn new(config: AwsKmsConfig) -> Result<Self, KeyProviderError>;
}

pub struct AwsKmsConfig {
    pub key_id: String,
    pub region: Option<String>,
    pub encryption_context: HashMap<String, String>,
}
```

**How It Works (Envelope Encryption):**
1. Generate random 32-byte MEK locally (one-time setup)
2. Encrypt MEK with KMS key (envelope encryption)
3. Store encrypted MEK in database metadata file (`encryption_metadata.bin`)
4. On startup, call KMS Decrypt to recover MEK
5. Derive component-specific DEKs from MEK using HKDF

**Benefits:**
- KMS key never leaves AWS KMS (hardware security boundary)
- MEK encrypted at rest, only decrypted in memory
- Automatic key rotation via KMS policies
- CloudTrail audit logging of key access
- IAM-based access control

#### 4. HashiCorp Vault Provider

```rust
/// Retrieves MEK from HashiCorp Vault
pub struct VaultKeyProvider {
    /// Vault HTTP client (TLS configuration consumed during creation)
    client: VaultClient,
    mount_path: String,
    secret_path: String,
    key_name: String,
}

impl VaultKeyProvider {
    /// Create provider from configuration
    /// Note: tls_config is consumed during VaultClient initialization
    pub async fn new(config: VaultConfig) -> Result<Self, KeyProviderError>;
}

pub struct VaultConfig {
    pub address: String,
    pub token: Option<String>,        // or use VAULT_TOKEN env
    pub mount_path: String,           // e.g., "secret" or "transit"
    pub secret_path: String,          // path within mount
    pub key_name: String,             // key field name
    /// TLS configuration (consumed during client creation)
    pub tls_config: Option<TlsConfig>,
}

/// TLS configuration for Vault connection
pub struct TlsConfig {
    /// Path to CA certificate bundle
    pub ca_cert: Option<PathBuf>,
    /// Skip TLS verification (DANGEROUS - only for testing)
    pub skip_verify: bool,
    /// Client certificate for mTLS
    pub client_cert: Option<PathBuf>,
    /// Client key for mTLS
    pub client_key: Option<PathBuf>,
}
```

**Supported Backends:**
- **KV v2**: Static secrets storage
- **Transit**: Encryption-as-a-service (envelope encryption)

#### 5. GCP Cloud KMS Provider

```rust
/// Retrieves MEK from Google Cloud KMS
pub struct GcpKmsProvider {
    client: kms::Client,
    key_name: String,  // projects/{}/locations/{}/keyRings/{}/cryptoKeys/{}
}
```

#### 6. Azure Key Vault Provider

```rust
/// Retrieves MEK from Azure Key Vault
pub struct AzureKeyVaultProvider {
    client: SecretClient,
    vault_url: String,
    secret_name: String,
}
```

### Encryption Scope

#### WAL Encryption

**Encrypted:** Each WAL entry's payload (operation data, properties, temporal info)
**Unencrypted:** Entry header (LSN, length, checksum placeholder)

```
WAL Entry Format (Encrypted):
┌─────────────────────────────────────────────────────────────────┐
│ Header (unencrypted)                                            │
│ ┌─────────┬─────────┬──────────┬───────────────────────────────┐│
│ │ LSN (8) │ Len (4) │ Type (1) │ Reserved (3)                  ││
│ └─────────┴─────────┴──────────┴───────────────────────────────┘│
├─────────────────────────────────────────────────────────────────┤
│ Encrypted Payload                                               │
│ ┌──────────────┬─────────────────────────────┬────────────────┐│
│ │ Nonce (12)   │ Ciphertext (variable)       │ Auth Tag (16)  ││
│ └──────────────┴─────────────────────────────┴────────────────┘│
├─────────────────────────────────────────────────────────────────┤
│ CRC32 (4) - covers entire entry including encrypted payload    │
└─────────────────────────────────────────────────────────────────┘
```

**Rationale:**
- LSN visible for recovery coordination (not sensitive)
- Length needed for segment parsing without decryption
- CRC32 covers encrypted data (integrity at storage layer)
- Auth tag provides cryptographic integrity (tampering detection)

**Implementation:**

```rust
impl EncryptedWalWriter {
    pub fn append(&mut self, entry: WalEntry) -> Result<LSN, WalError> {
        // 1. Serialize entry payload
        let plaintext = entry.serialize_payload()?;

        // 2. Encrypt with WAL DEK
        let ciphertext = self.cipher.encrypt(&plaintext)?;

        // 3. Write header + encrypted payload
        let header = WalEntryHeader {
            lsn: self.next_lsn(),
            len: ciphertext.len() as u32,
            entry_type: entry.entry_type(),
        };

        self.write_header(&header)?;
        self.write_bytes(&ciphertext)?;

        // 4. Write CRC32 of entire entry
        let crc = compute_crc(&header, &ciphertext);
        self.write_u32(crc)?;

        Ok(header.lsn)
    }
}
```

#### Index File Encryption

**Encrypted:** Entire index file content after magic bytes and version
**Unencrypted:** Magic bytes (4), version (2), encryption metadata

```
Index File Format (Encrypted):
┌─────────────────────────────────────────────────────────────────┐
│ Header (unencrypted)                                            │
│ ┌──────────────┬───────────┬─────────────────────────────────┐ │
│ │ Magic (4)    │ Version(2)│ Encryption Header (variable)    │ │
│ │ "GIDX"       │ 0x0002    │ (algorithm, key_id, flags)      │ │
│ └──────────────┴───────────┴─────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────────┤
│ Encrypted Content                                               │
│ ┌──────────────┬─────────────────────────────┬────────────────┐│
│ │ Nonce (12)   │ Ciphertext (variable)       │ Auth Tag (16)  ││
│ └──────────────┴─────────────────────────────┴────────────────┘│
├─────────────────────────────────────────────────────────────────┤
│ CRC32 (4) - covers header + encrypted content                  │
└─────────────────────────────────────────────────────────────────┘
```

**Encryption Header:**

```rust
#[derive(Encode, Decode)]
pub struct EncryptionHeader {
    /// Algorithm identifier
    pub algorithm: u8,        // 0 = None, 1 = AES-256-GCM, 2 = ChaCha20-Poly1305

    /// Key version identifier (UUID v4 raw bytes)
    /// Used to identify which MEK version was used for encryption.
    /// On key rotation, a new UUID is generated for the new MEK.
    /// During decryption, this ID determines which key to use from
    /// the key version cache (supports reading data encrypted with
    /// previous key versions during rotation).
    pub key_version_id: [u8; 16],

    /// Additional flags (reserved for future use)
    /// Bit 0: Compressed before encryption
    /// Bits 1-7: Reserved
    pub flags: u8,
}

impl EncryptionHeader {
    /// Size of the encryption header in bytes
    pub const SIZE: usize = 1 + 16 + 1; // 18 bytes

    /// Create header for new encryption
    pub fn new(algorithm: u8, key_version_id: [u8; 16]) -> Self {
        Self {
            algorithm,
            key_version_id,
            flags: 0,
        }
    }
}
```

#### Cold Storage (Redb) Encryption

**Encrypted:** Each stored value (NodeVersion/EdgeVersion bytes)
**Unencrypted:** Redb internal structure, table names, keys (version IDs)

```rust
impl EncryptedRedbColdStorage {
    fn store_node_version(&self, version: &NodeVersion) -> Result<(), ColdStorageError> {
        // 1. Serialize and compress (existing flow)
        let serialized = bitcode::encode(version)?;
        let compressed = zstd::encode_all(&serialized[..], self.compression_level)?;

        // 2. Encrypt compressed data
        let encrypted = self.cipher.encrypt(&compressed)?;

        // 3. Store in Redb
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(NODE_VERSIONS)?;
            table.insert(version.id.0, encrypted.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}
```

**Order of Operations:**
1. Serialize (bitcode)
2. Compress (zstd)
3. Encrypt (AES-256-GCM)

**Rationale:** Compress before encrypt for better compression ratios (encrypted data doesn't compress).

#### Checkpoint Encryption

**Encrypted:** Checkpoint data after header
**Unencrypted:** Magic bytes, version, encryption metadata

Same pattern as index files.

### Threat Model

#### In-Scope Threats (Protected Against)

| Threat | Mitigation |
|--------|------------|
| **Disk theft** | All persisted data encrypted with AES-256-GCM |
| **Unauthorized file access** | Decryption requires MEK from key provider |
| **Data tampering** | AEAD authentication tags detect modification |
| **Key compromise (single DEK)** | Compartmentalized DEKs limit blast radius |
| **Backup exposure** | Backups remain encrypted without MEK |
| **Cloud storage breach** | Data at rest encrypted before cloud upload |

#### Out-of-Scope Threats (Not Protected)

| Threat | Reason | Recommendation |
|--------|--------|----------------|
| **In-memory attacks** | Data decrypted for processing | Use memory-safe Rust, consider memory encryption |
| **Side-channel attacks** | Out of scope for v1 | Use ChaCha20 for timing-sensitive environments |
| **Root/admin access** | MEK accessible to admin | Use HSM/KMS with access controls |
| **Denial of service** | Encryption doesn't prevent DoS | Use existing DoS protections |
| **Traffic analysis** | File sizes/access patterns visible | Out of scope |
| **Quantum attacks** | AES-256 reduced to 128-bit effective security by Grover's algorithm | Monitor PQC standardization; plan migration path |

#### Key Security Requirements

1. **MEK Protection**
   - Never stored in plaintext on disk (except file provider for dev)
   - Zeroized from memory when no longer needed (`zeroize` crate)
   - Access logged when using KMS providers

2. **Nonce Management**
   - Random 96-bit nonces for each encryption operation
   - CSPRNG (`rand::thread_rng()` backed by OS entropy)
   - No nonce reuse (cryptographically improbable with random nonces)

3. **Key Rotation**
   - New MEK generates new DEKs
   - Old DEKs retained for reading existing data
   - Background re-encryption of existing data (optional)

### Configuration

```rust
/// Encryption configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptionConfig {
    /// Enable encryption at rest
    pub enabled: bool,

    /// Encryption algorithm
    pub algorithm: Algorithm,

    /// Key provider configuration
    pub key_provider: KeyProviderConfig,

    /// Components to encrypt (default: all)
    pub components: EncryptedComponents,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Algorithm {
    /// AES-256-GCM (hardware accelerated)
    Aes256Gcm,

    /// ChaCha20-Poly1305 (constant-time)
    ChaCha20Poly1305,

    /// Auto-select based on hardware capabilities
    Auto,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum KeyProviderConfig {
    /// Read key from file
    File { path: PathBuf, format: KeyFormat },

    /// Read key from environment variable
    Env { var_name: String, format: KeyFormat },

    /// AWS KMS
    AwsKms {
        key_id: String,
        region: Option<String>,
        encryption_context: HashMap<String, String>,
    },

    /// HashiCorp Vault
    Vault {
        address: String,
        mount_path: String,
        secret_path: String,
        key_name: String,
    },

    /// GCP Cloud KMS
    GcpKms { key_name: String },

    /// Azure Key Vault
    AzureKeyVault {
        vault_url: String,
        secret_name: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedComponents {
    pub wal: bool,
    pub indexes: bool,
    pub cold_storage: bool,
    pub checkpoints: bool,
}

impl Default for EncryptedComponents {
    fn default() -> Self {
        Self {
            wal: true,
            indexes: true,
            cold_storage: true,
            checkpoints: true,
        }
    }
}
```

**TOML Configuration Example:**

```toml
[encryption]
enabled = true
algorithm = "auto"

[encryption.key_provider]
type = "aws_kms"
key_id = "arn:aws:kms:us-east-1:123456789:key/12345678-1234-1234-1234-123456789012"
region = "us-east-1"

[encryption.key_provider.encryption_context]
service = "aletheiadb"
environment = "production"

[encryption.components]
wal = true
indexes = true
cold_storage = true
checkpoints = true
```

### Performance Considerations

**Expected Overhead:**
| Operation | Without Encryption | With Encryption | Overhead |
|-----------|-------------------|-----------------|----------|
| WAL append | ~100ns | ~150ns | ~50% |
| WAL batch (100 entries) | ~2μs | ~2.5μs | ~25% |
| Index save (1M nodes) | ~2s | ~2.1s | ~5% |
| Cold storage write | ~100μs | ~110μs | ~10% |
| Current-state query | <1μs | <1μs | ~0% (not encrypted) |

**Optimization Strategies:**

1. **Batch Encryption**: Encrypt multiple WAL entries in one operation
2. **Parallel Encryption**: Use thread pool for large index files
3. **Hardware Acceleration**: AES-NI provides 3-5 GB/s throughput
4. **In-Memory Caching**: Decrypted data cached in memory for queries

**Benchmark Requirements:**
- WAL throughput: >90% of unencrypted baseline
- Index load time: <10% overhead
- Memory usage: <5% increase (for encryption buffers)

**Required Benchmarks:**
- Encrypted vs unencrypted WAL throughput (single entry, batch 10, batch 100, batch 1000)
- CPU profiling with and without AES-NI
- Latency percentiles (p50, p95, p99) for encrypted operations
- Memory pressure under high encryption load

### Metrics and Observability

**Encryption Metrics (exposed via Prometheus):**

```rust
/// Metrics for encryption operations
pub struct EncryptionMetrics {
    /// Total encryption operations
    pub encrypt_total: Counter,
    /// Total decryption operations
    pub decrypt_total: Counter,
    /// Encryption errors
    pub encrypt_errors: Counter,
    /// Decryption errors (including auth failures)
    pub decrypt_errors: Counter,
    /// Encryption latency histogram
    pub encrypt_duration: Histogram,
    /// Decryption latency histogram
    pub decrypt_duration: Histogram,
    /// Bytes encrypted
    pub bytes_encrypted: Counter,
    /// Bytes decrypted
    pub bytes_decrypted: Counter,
    /// Key provider health status (1 = healthy, 0 = unhealthy)
    pub key_provider_healthy: Gauge,
    /// Current key version age in seconds
    pub key_version_age_seconds: Gauge,
}
```

**Key Metrics to Monitor:**

| Metric | Alert Threshold | Description |
|--------|----------------|-------------|
| `encrypt_errors` | >0 | Any encryption failure is critical |
| `decrypt_errors` | >0 | Auth failures may indicate tampering |
| `key_provider_healthy` | =0 | KMS connectivity issues |
| `key_version_age_seconds` | >7776000 (90 days) | Key rotation overdue |
| `encrypt_duration_p99` | >1ms | Performance degradation |

**Audit Logging:**

All key access operations are logged:
```json
{
  "event": "mek_access",
  "timestamp": "2026-01-27T10:00:00Z",
  "provider": "aws_kms",
  "key_version_id": "550e8400-e29b-41d4-a716-446655440000",
  "operation": "decrypt",
  "success": true,
  "latency_ms": 45
}
```

## Consequences

### Positive

1. **Regulatory Compliance**: Meets HIPAA, PCI-DSS, GDPR encryption requirements
2. **Defense in Depth**: Additional security layer beyond access controls
3. **Secure Backups**: Backups encrypted without additional tooling
4. **Key Management Flexibility**: From simple files to enterprise HSMs
5. **Minimal Performance Impact**: AES-NI hardware acceleration
6. **Component Isolation**: Separate DEKs limit breach impact

### Negative

1. **Operational Complexity**: Key management adds deployment complexity
2. **Recovery Dependencies**: Lost MEK = unrecoverable data
3. **Debugging Difficulty**: Can't inspect encrypted files directly
4. **Performance Overhead**: ~5-10% for I/O-heavy workloads
5. **External Dependencies**: KMS providers require network access

### Neutral

1. **Current-state queries unaffected**: In-memory data not encrypted
2. **Compression still effective**: Applied before encryption
3. **CRC32 still works**: Covers encrypted data
4. **Existing file formats extended**: Backwards-compatible with version field

## Alternatives Considered

### Alternative 1: Full-Disk Encryption Only (dm-crypt/LUKS, BitLocker)

**Pros:**
- Zero application changes
- Transparent to database
- Widely supported

**Cons:**
- No per-tenant isolation in multi-tenant scenarios
- Key management external to application
- All-or-nothing (no selective encryption)
- Doesn't protect against authorized user misuse

**Decision:** Rejected - doesn't meet multi-tenant or compliance audit requirements

### Alternative 2: XChaCha20-Poly1305

**Pros:**
- 24-byte nonces (vs 12-byte) - eliminates nonce collision concerns
- Same performance as ChaCha20-Poly1305

**Cons:**
- Less widely implemented/audited
- Not in `ring` crate (would need `chacha20poly1305` crate)
- Nonce collision already cryptographically improbable with random 96-bit nonces

**Decision:** Rejected - additional complexity without meaningful security benefit for our use case

### Alternative 3: Single DEK for All Components

**Pros:**
- Simpler key management
- Less cryptographic material to protect

**Cons:**
- Single point of failure (one DEK compromise exposes everything)
- Can't rotate keys independently per component
- Harder to implement selective encryption

**Decision:** Rejected - compartmentalization provides better security posture

### Alternative 4: Transparent Data Encryption (TDE) at Storage Layer

**Pros:**
- Transparent to application code
- Single encryption point

**Cons:**
- Requires modifying all storage backends
- Harder to integrate with existing CRC32/checksums
- Less flexibility in encryption granularity

**Decision:** Rejected - layer-specific encryption provides better control

### Alternative 5: Client-Side Encryption Only

**Pros:**
- Data encrypted before reaching database
- Database never sees plaintext

**Cons:**
- Can't query encrypted data
- Breaks temporal queries (need to compare values)
- Client must manage all encryption
- Doesn't protect metadata

**Decision:** Rejected - incompatible with query functionality

## Implementation Plan

### Phase 1: Core Infrastructure

- [ ] Add `aes-gcm` crate for AES-256-GCM
- [ ] Add `chacha20poly1305` crate for ChaCha20-Poly1305
- [ ] Add `hkdf` and `sha2` crates for key derivation
- [ ] Add `zeroize` crate for secure memory handling
- [ ] Implement `Cipher` trait and algorithm implementations
- [ ] Implement `KeyProvider` trait
- [ ] Implement `FileKeyProvider` and `EnvKeyProvider`
- [ ] Add `EncryptionConfig` to configuration system

### Phase 2: WAL Encryption

- [ ] Extend WAL entry format with encryption support
- [ ] Implement `EncryptedWalWriter`
- [ ] Implement `EncryptedWalReader`
- [ ] Update WAL recovery to handle encrypted entries
- [ ] Add benchmarks for encrypted WAL throughput

### Phase 3: Index Encryption

- [ ] Extend index file format with encryption header
- [ ] Implement encrypted save/load for manifest
- [ ] Implement encrypted save/load for graph index
- [ ] Implement encrypted save/load for temporal index
- [ ] Implement encrypted save/load for vector index metadata
- [ ] Update parallel loading for encrypted indexes

### Phase 4: Cold Storage Encryption

- [ ] Implement `EncryptedRedbColdStorage`
- [ ] Update migration service for encryption
- [ ] Add encryption to checkpoint files
- [ ] Test recovery with encrypted cold storage

### Phase 5: KMS Integration

- [ ] Implement `AwsKmsProvider` (feature-gated)
- [ ] Implement `VaultKeyProvider` (feature-gated)
- [ ] Implement `GcpKmsProvider` (feature-gated)
- [ ] Implement `AzureKeyVaultProvider` (feature-gated)
- [ ] Add integration tests with LocalStack/Vault dev mode

### Phase 6: Key Rotation

- [ ] Leverage existing `key_version_id` field in encryption headers for version tracking
- [ ] Implement key version cache for reading data encrypted with previous MEKs
- [ ] Implement background re-encryption service for migrating to new keys
- [ ] Add key rotation commands to CLI (`aletheiadb key rotate`, `aletheiadb key status`)
- [ ] Document key rotation procedures

**Key Rotation Triggers:**
- **Time-based**: Recommended every 90 days (configurable)
- **Operation-based**: After 2^32 encryptions with same DEK (GCM nonce safety margin)
- **Incident-based**: Immediately upon suspected key compromise

### Phase 7: Documentation & Security Review

- [ ] Write user guide (`docs/ENCRYPTION.md`)
- [ ] Add configuration examples
- [ ] Security review by external party
- [ ] Penetration testing of encrypted storage

### Phase 8: Testing

- [ ] Unit tests for cipher implementations (encrypt/decrypt round-trip)
- [ ] Property-based tests using `proptest` (arbitrary data encryption)
- [ ] Fault injection tests (corrupted ciphertext, truncated files)
- [ ] KMS mocking with LocalStack (AWS) and Vault dev mode
- [ ] Crash recovery tests with encrypted WAL/indexes
- [ ] Nonce uniqueness verification tests (statistical analysis)
- [ ] Key rotation integration tests
- [ ] Performance regression tests (encrypted vs unencrypted)

## References

**Standards:**
- [NIST SP 800-38D: GCM Mode](https://csrc.nist.gov/publications/detail/sp/800-38d/final)
- [RFC 8439: ChaCha20 and Poly1305](https://datatracker.ietf.org/doc/html/rfc8439)
- [RFC 5869: HKDF](https://datatracker.ietf.org/doc/html/rfc5869)

**Rust Crates:**
- [aes-gcm](https://docs.rs/aes-gcm/) - AES-256-GCM implementation
- [chacha20poly1305](https://docs.rs/chacha20poly1305/) - ChaCha20-Poly1305 implementation
- [hkdf](https://docs.rs/hkdf/) - HMAC-based Key Derivation Function
- [zeroize](https://docs.rs/zeroize/) - Securely zero memory

**Key Management:**
- [AWS KMS Developer Guide](https://docs.aws.amazon.com/kms/latest/developerguide/)
- [HashiCorp Vault Transit Secrets Engine](https://developer.hashicorp.com/vault/docs/secrets/transit)
- [OWASP Cryptographic Storage Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Cryptographic_Storage_Cheat_Sheet.html)

**Related ADRs:**
- ADR-0007: WAL Durability
- ADR-0023: Index Persistence Layer
- ADR-0025: Redb Cold Storage
- Issue #476: Design encryption-at-rest architecture
