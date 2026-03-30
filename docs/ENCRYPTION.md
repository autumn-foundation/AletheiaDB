# Encryption at Rest

This document describes how to configure and use AletheiaDB's encryption-at-rest
feature, which protects all persisted data from unauthorized access to storage
media.

## Overview

AletheiaDB encrypts data across all persistence layers:

- **WAL segments** -- Transaction log entries (payload encrypted, headers as AAD)
- **Index files** -- Graph structure, vector embeddings, temporal version chains
- **Cold storage (Redb)** -- Compressed historical versions
- **Checkpoint files** -- Database state snapshots

### Threat Model

Encryption at rest protects against:

1. **Unauthorized disk access** -- Stolen drives, decommissioned hardware, backups
2. **Compliance requirements** -- HIPAA, PCI-DSS, GDPR, SOC2 mandates
3. **Multi-tenant isolation** -- Shared storage environments
4. **LLM knowledge leakage** -- Knowledge graphs may contain sensitive business data

Encryption at rest does **not** protect against:

- Compromised database process memory (use OS-level protections)
- Network-level attacks (use TLS for transport encryption)
- Authorized users with database access (use access control)

## Quick Start

### 1. Generate a Master Key

```bash
# Using the CLI (when integrated into a binary)
aletheiadb keys generate --output /etc/aletheiadb/master.key
```

Or programmatically:

```rust
use aletheiadb::encryption::cli::generate_key;
use std::path::Path;

let result = generate_key(Path::new("/etc/aletheiadb/master.key"))?;
println!("Key written to: {}", result.path);
println!("Key length: {} bytes", result.key_length);
```

### 2. Set File Permissions

```bash
chmod 600 /etc/aletheiadb/master.key
chown aletheiadb:aletheiadb /etc/aletheiadb/master.key
```

### 3. Enable Encryption in Configuration

**TOML configuration:**

```toml
[encryption]
enabled = true
algorithm = "auto"

[encryption.key_provider]
type = "file"
path = "/etc/aletheiadb/master.key"
```

**Programmatic configuration:**

```rust
use aletheiadb::{AletheiaDB, config::AletheiaDBConfig};
use aletheiadb::encryption::config::EncryptionConfig;

let config = AletheiaDBConfig::builder()
    .encryption(EncryptionConfig::file_based("/etc/aletheiadb/master.key"))
    .build();

let db = AletheiaDB::with_unified_config(config)?;
```

## Key Management

### File Provider

Reads the Master Encryption Key (MEK) from a file on disk. Supports two formats:

| Format | Description | File Size |
|--------|-------------|-----------|
| Hex | 64 lowercase hex characters (with optional trailing newline) | 64-65 bytes |
| Binary | Raw 32-byte key | 32 bytes |

The format is auto-detected based on file content.

```toml
[encryption.key_provider]
type = "file"
path = "/etc/aletheiadb/master.key"
```

### Environment Variable Provider

Reads the MEK from an environment variable. The value must be a 64-character
hex-encoded string.

```toml
[encryption.key_provider]
type = "env"
variable = "ALETHEIADB_MEK"
```

```bash
export ALETHEIADB_MEK="a1b2c3d4e5f6...64 hex chars..."
```

This is useful for container deployments where secrets are injected as environment
variables (Kubernetes Secrets, Docker secrets, AWS ECS task definitions).

### Generating Keys

Generate a new random 256-bit key file:

```rust
use aletheiadb::encryption::key_provider::FileKeyProvider;
use std::path::Path;

let key = FileKeyProvider::generate_key_file(Path::new("master.key"))?;
// Key is written as 64 hex chars + newline
// Parent directories are created automatically
```

The generated key is cryptographically random (sourced from the OS CSPRNG via
`rand::thread_rng`).

### Validating Keys

Verify that a key file exists and contains a valid key:

```rust
use aletheiadb::encryption::cli::validate_key_file;
use std::path::Path;

validate_key_file(Path::new("/etc/aletheiadb/master.key"))?;
```

## Configuration Reference

### Full Configuration

```toml
[encryption]
# Enable or disable encryption at rest (default: false)
enabled = true

# Algorithm selection (default: "auto")
# Options: "auto", "aes-256-gcm", "chacha20-poly1305"
algorithm = "auto"

[encryption.key_provider]
# Provider type: "file" or "env"
type = "file"

# For "file" provider: path to the key file
path = "/etc/aletheiadb/master.key"

# For "env" provider: environment variable name
# variable = "ALETHEIADB_MEK"
```

### Algorithm Selection

| Algorithm | When to Use |
|-----------|-------------|
| `auto` | **Recommended.** Selects AES-256-GCM on x86/x86_64 with AES-NI hardware, ChaCha20-Poly1305 otherwise. |
| `aes-256-gcm` | Force AES-256-GCM. Best performance on CPUs with AES-NI. |
| `chacha20-poly1305` | Force ChaCha20-Poly1305. Consistent performance without hardware acceleration. |

Both algorithms provide authenticated encryption (AEAD), ensuring both
confidentiality and integrity of encrypted data.

### Programmatic Configuration

```rust
use aletheiadb::encryption::config::{EncryptionConfig, KeyProviderConfig};
use aletheiadb::encryption::factory::Algorithm;
use std::path::PathBuf;

// File-based with explicit algorithm
let config = EncryptionConfig {
    enabled: true,
    algorithm: Algorithm::Aes256Gcm,
    key_provider: KeyProviderConfig::File {
        path: PathBuf::from("/etc/aletheiadb/master.key"),
    },
};

// Env-based with auto algorithm
let config = EncryptionConfig::env_based("ALETHEIADB_MEK");

// Disabled (default)
let config = EncryptionConfig::disabled();
```

## Architecture

### Key Hierarchy

AletheiaDB uses a two-level key hierarchy to limit the blast radius of any
single key compromise:

```
KeyProvider (file / env)
    |
    v
Master Encryption Key (MEK) -- 256-bit, from provider
    |
    +-- HKDF-SHA256 (info="aletheiadb-wal-dek-v1")        --> WAL DEK
    +-- HKDF-SHA256 (info="aletheiadb-index-dek-v1")      --> Index DEK
    +-- HKDF-SHA256 (info="aletheiadb-cold-dek-v1")       --> Cold Storage DEK
    +-- HKDF-SHA256 (info="aletheiadb-checkpoint-dek-v1") --> Checkpoint DEK
```

- The **MEK** never encrypts data directly. It is only used as input to HKDF.
- Each storage component gets a **unique DEK** derived from the MEK.
- Compromising one DEK does not reveal the MEK or other DEKs.
- The `v1` suffix in the info string allows future key-schedule versioning.

### What Gets Encrypted Per Layer

| Layer | Encrypted | Plaintext (AAD) | Notes |
|-------|-----------|-----------------|-------|
| WAL | Payload bytes (offset 25+) | 24-byte header + 1-byte op type | Header is authenticated via AAD |
| Index files | Full serialized content | -- | Encrypted before writing to disk |
| Cold storage | Historical version data | -- | Encrypted before Redb insertion |
| Checkpoints | Full snapshot data | -- | Encrypted before writing to disk |

### Encryption Manager

The `EncryptionManager` is created once at database startup and shared (via
`Arc`) with all persistence subsystems:

```rust
use aletheiadb::encryption::manager::EncryptionManager;
use aletheiadb::encryption::config::EncryptionConfig;

let config = EncryptionConfig::file_based("master.key");
let manager = EncryptionManager::from_config(&config)?;

// Obtain per-component ciphers
let wal_cipher = manager.wal_cipher();
let index_cipher = manager.index_cipher();
let cold_cipher = manager.cold_cipher();
let checkpoint_cipher = manager.checkpoint_cipher();
```

## Key Rotation

AletheiaDB provides a foundation for key rotation via the `KeyRotationManager`.
Key rotation allows switching to a new MEK without database downtime.

### Rotation Process

1. **Begin rotation** -- Allocates a new key version, marks state as `InProgress`.
2. **New writes use the new key** -- All new encryptions use the updated DEKs.
3. **Complete rotation** -- Marks rotation as `Complete`.

```rust
use aletheiadb::encryption::rotation::KeyRotationManager;

let rotation = KeyRotationManager::new();

// Check current version
let version = rotation.current_version(); // 1

// Begin rotation (returns new version number)
let new_version = rotation.begin_rotation()?; // 2

// ... deploy new key, update config ...

rotation.complete_rotation()?;
```

**Note:** Background re-encryption of existing data written with the old key is
planned for a future phase. Currently, rotation applies only to new writes.

## Audit Logging

Encryption operations emit structured audit events for compliance tracking.

### Audit Levels

| Level | Events Logged | Use Case |
|-------|--------------|----------|
| `None` | Nothing | Development, testing |
| `KeyEvents` (default) | Key load, rotation start/complete/fail, access denied | Production |
| `AllOperations` | All of the above + every encrypt/decrypt call | Compliance audits (high volume) |

### Audit Events

| Event | Level | Description |
|-------|-------|-------------|
| `KeyLoaded` | KeyEvents | MEK loaded at startup (provider name, key version) |
| `RotationStarted` | KeyEvents | Key rotation initiated (old and new version) |
| `RotationCompleted` | KeyEvents | Key rotation finished (new version, duration) |
| `RotationFailed` | KeyEvents | Key rotation error (version, error message) |
| `KeyAccessDenied` | KeyEvents | Provider rejected key access (provider, error) |
| `EncryptOperation` | AllOperations | Encryption performed (component, key version) |
| `DecryptOperation` | AllOperations | Decryption performed (component, key version) |

### Configuration

Audit logging is configured on the `EncryptionAuditLogger`:

```rust
use aletheiadb::encryption::audit::{EncryptionAuditLogger, AuditLevel};

let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "node-1");
```

## Performance

Encryption adds minimal overhead to persistence operations. Both supported
algorithms are designed for high throughput:

| Algorithm | Typical Throughput | Hardware |
|-----------|--------------------|----------|
| AES-256-GCM | >1 GB/s | x86_64 with AES-NI |
| ChaCha20-Poly1305 | >500 MB/s | Software-only (ARM, older x86) |

### Per-Layer Overhead

| Layer | Expected Overhead | Notes |
|-------|-------------------|-------|
| WAL | <3% throughput reduction | Only payload is encrypted; header stays plaintext |
| Index persistence | <5% write time increase | One-time cost at save; no impact on in-memory queries |
| Cold storage | <5% write time increase | Encryption piggybacks on existing serialization |
| Checkpoints | <5% write time increase | Encrypted during periodic snapshot writes |

Current-state queries (in-memory lookups) are **not affected** by encryption
since data is decrypted when loaded into memory.

## Security Considerations

1. **Protect the key file.** Set file permissions to `600` (owner read/write only).
   The key file is the single point of trust for all encrypted data.

2. **Never commit keys to version control.** Add key files to `.gitignore`.

3. **Use environment variables in containers.** For Docker/Kubernetes deployments,
   inject the MEK via secrets management rather than mounting key files.

4. **Back up your key separately from data.** If the key is lost, encrypted data
   is unrecoverable. Store key backups in a separate secure location.

5. **Memory safety.** All key material uses `Zeroizing<[u8; 32]>` wrappers that
   securely erase memory when dropped, preventing key leakage through memory dumps.

6. **Authenticated encryption.** Both AES-256-GCM and ChaCha20-Poly1305 are AEAD
   ciphers. Any tampering with encrypted data (or AAD) is detected during decryption.

7. **Per-component isolation.** Each storage layer uses a separate DEK derived via
   HKDF. Compromising one component's key material does not affect others.

8. **Nonce management.** Each encryption operation generates a fresh random nonce.
   Nonce reuse is avoided by using the OS CSPRNG.

## Troubleshooting

### "Key not found" error at startup

The configured key file or environment variable does not exist.

- **File provider:** Verify the path in your config matches the actual key file
  location. Check that the file is readable by the database process.
- **Env provider:** Verify the environment variable is set in the process
  environment (not just in a shell that spawned it).

### "Invalid key format" error

The key file exists but does not contain a valid key.

- Hex keys must be exactly 64 hex characters (lowercase or uppercase).
- Binary keys must be exactly 32 bytes.
- Whitespace and trailing newlines are trimmed before parsing.

### "Decryption failed" error

Data was encrypted with a different key than the one currently configured.
This can happen after:

- Replacing the key file without re-encrypting existing data.
- Restoring a backup encrypted with a different key.
- Key file corruption.

Verify that the current key matches the key used when the data was written.

### Performance degradation

If encryption overhead exceeds expected values:

- Verify that AES-NI is available on your CPU (`lscpu | grep aes` on Linux).
- If AES-NI is not available, use `algorithm = "chacha20-poly1305"` explicitly
  rather than `auto` to avoid the detection overhead.
- Check that the `auto` algorithm resolved to the expected cipher by inspecting
  the `EncryptionManager` debug output or audit log.

### Checking encryption status

Use the CLI helpers to inspect the current configuration:

```rust
use aletheiadb::encryption::cli::{get_encryption_status, format_encryption_status};
use aletheiadb::encryption::config::EncryptionConfig;

let config = EncryptionConfig::file_based("master.key");
let status = get_encryption_status(&config);
print!("{}", format_encryption_status(&status));
```

Output:

```
Encryption Status
---------------------------------
Overall:        ENABLED
Algorithm:      Auto (AES-256-GCM if AES-NI, else ChaCha20-Poly1305)
Key Provider:   file (master.key)
```
