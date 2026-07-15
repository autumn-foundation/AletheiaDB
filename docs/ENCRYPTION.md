# Encryption at Rest

This document describes how to configure and use AletheiaDB's encryption-at-rest
feature, which protects all persisted data from unauthorized access to storage
media.

## Overview

AletheiaDB encrypts data across all persistence layers:

- **WAL segments** -- Transaction log entries (payload encrypted, headers as AAD)
- **Index files** -- The persisted graph, temporal version chains, string
  interner, manifest, temporal-adjacency index, each vector index's metadata
  and NodeId↔key mappings, **and the native HNSW `current.usearch` graph file
  plus its `current.usearch.mappings` sidecar** (Issue #481). Each file is
  encrypted **whole-file** (the entire serialized/compressed buffer) behind a
  small plaintext detection header.
- **Cold storage (Redb)** -- Compressed historical versions
- **Checkpoint files** -- Database state snapshots (the [`CheckpointManager`]
  threads the same index cipher into its `IndexPersistenceManager`, so
  checkpoints write and restore their graph/temporal/interner/manifest files
  encrypted — construct it with `CheckpointManager::with_cipher`).

> **Native HNSW encryption (Issue #481):** the native HNSW graph file
> (`current.usearch`) and its `current.usearch.mappings` sidecar are written
> and memory-mapped directly by the bundled `usearch` C++ library through a
> filesystem path (it has no in-memory-buffer API). To encrypt them at rest we
> let usearch produce its plaintext files at a temporary path in the same
> directory, whole-file-encrypt each (with the `AEIX` header as AAD), and
> atomically publish the ciphertext to the real paths; on load an
> `AEIX`-headed native file is decrypted to a temp file that is handed to
> usearch and then removed (a drop guard cleans up on every exit path).
> Encrypting the native file necessarily **forgoes mmap** for that index (a
> ciphertext cannot be mmapped meaningfully) — it is decrypted to a temp file
> and loaded. When encryption is disabled the native path is byte-identical to
> a plain `index.save`/`HnswIndex::load`, and a legacy plaintext
> `current.usearch` (no header) still loads when a cipher is configured (the
> upgrade scenario), via first-byte header sniffing.

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
- **A filesystem-WRITE adversary (known limitation).** Encryption at rest
  defends against a **read-only** adversary (a stolen disk or backup). The
  *encryption-enabled* state is not itself authenticated on disk: every index
  file is individually AEAD-authenticated (tampering a ciphertext or its `AEIX`
  header is detected), but nothing records that a component *must* be
  encrypted. An adversary who can **write** to the storage directory could
  therefore substitute a forged **plaintext** index file (no `AEIX` header),
  which the header-sniffing loader would accept as a legacy plaintext file. A
  persisted "encryption-required" marker that makes the loader reject an
  unexpectedly-plaintext file is a tracked follow-up; it is **not** implemented
  today. Mitigate with OS-level write protection / integrity monitoring on the
  data directory.

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
| Index files | Full serialized/compressed buffer (incl. CRC) | 10-byte header (`AEIX`, format, algorithm id, key version) | Whole-file AEAD; header is authenticated via AAD. Includes the native `current.usearch` HNSW file + `.usearch.mappings` sidecar (decrypt-to-temp shuffle around the native loader; Issue #481). Encrypted files can't be mmapped, so `use_mmap` graph reads and the native HNSW load fall back to buffered decrypt. |
| Cold storage | Historical version data | -- | Encrypted before Redb insertion |
| Checkpoints | Full snapshot data | -- | Encrypted before writing to disk |

### Enabling Encryption Over an Existing Dataset (Lazy Migration)

Turning encryption on over an **existing plaintext** dataset does **not**
proactively re-encrypt the index files already on disk. Migration is **lazy**:

- On startup the header-sniffing loader reads each existing plaintext index
  file correctly even with a cipher configured (the upgrade/mixed-directory
  contract), so no data is lost.
- Each index file is only rewritten **encrypted** on its **next
  mutation-gated save** (the persistence cycle that re-serializes that
  component). Until then it remains plaintext on disk. A directory can
  therefore legitimately hold a mix of plaintext and encrypted index files.
- To force a **full re-encrypt immediately**, call
  [`AletheiaDB::persist_indexes()`], which rewrites **every** on-disk index
  file — including the temporal-adjacency index and the native
  `current.usearch` HNSW files — through the cipher in one pass.

> **The encryption algorithm cannot be changed in place.** The active
> algorithm's 1-byte id is written into every file's `AEIX` header and fed to
> the cipher as AAD. Switching `algorithm` (e.g. `aes-256-gcm` →
> `chacha20-poly1305`) over a dataset that already has encrypted files makes
> those files **unreadable**: the load path reports a clear `algorithm
> mismatch (file=<id>, configured=<id>)` error (Issue #481) rather than a
> generic decryption failure. To change algorithms you must re-encrypt from a
> plaintext export / backup taken under the old algorithm.

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
| Index persistence | <5% write time increase | One-time cost at save; no impact on in-memory queries. Encrypted index files are read whole and decrypted into a buffer, so `use_mmap` graph reads fall back to buffered decrypt (no live mmap of ciphertext). |
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

8. **Nonce management.** Each encryption operation generates a fresh random
   nonce from the OS CSPRNG. AES-256-GCM uses a random **96-bit** nonce, which
   carries a birthday-bound safe limit of roughly **2^32 invocations per key**
   before the collision probability becomes non-negligible. This is not a
   practical concern for index persistence: index writes are **infrequent**
   (one whole-file encryption per component per mutation-gated save, not
   per-record), and each storage component uses a **distinct DEK** derived via
   HKDF (WAL / index / cold / checkpoint), so the per-key invocation count for
   the index DEK stays far below the bound. For sustained high-churn workloads,
   periodic **key rotation** (Issue #488) resets the per-key counter. (ChaCha20-
   Poly1305 also uses a 96-bit random nonce with the same guidance.)

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

A single **unreadable index file never bricks startup.** An encrypted index
file that cannot be decrypted (wrong/missing key, corruption, or a truncated
crash-during-save) is treated exactly like a corrupt *plaintext* index file:
the loader logs a warning (`index '<name>' load failed, recovering from WAL`)
and falls back to reconstructing that component from the Write-Ahead Log. The
warning text never contains key material.

> **Recovery property (pre-existing, not an encryption regression).** Index
> recovery is differential: the manifest records the LSN floor from which WAL
> replay resumes. If the **manifest survives** but a component's **snapshot**
> (e.g. `graph/adjacency.idx`) is unreadable, differential replay starts at the
> manifest LSN — so any pre-snapshot data that lived **only** in that snapshot
> (whose creating WAL entries fall below the manifest LSN and have been
> truncated/compacted) is lost for that component. This is identical to how a
> corrupt *plaintext* snapshot with a surviving manifest behaves; encryption
> does not change it. When the manifest **itself** is also unreadable, startup
> discards the LSN floor and falls back to full WAL replay from the beginning,
> recovering everything still present in the WAL.

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

## CLI Operator Commands (Issue #490)

The `aletheia` binary exposes encryption operator commands. Key bytes and
passphrases are **never** printed by any of them. Commands that inspect or
operate on a live database open it from the ambient configuration
(`ALETHEIADB_CONFIG` TOML, or `ALETHEIADB_DATA_DIR`).

### `keys` — key material

```bash
# Provision a new 32-byte master key (0600 on Unix; refuses overwrite w/o --force)
aletheia keys generate --output /etc/aletheiadb/master.key

# Show provider / algorithm / key version (no key material printed). Alias: info
aletheia keys status --key-file /etc/aletheiadb/master.key

# Health-check that a key file loads and is valid
aletheia keys verify --key-file /etc/aletheiadb/master.key
```

### `keys rotate` — index key rotation (engine: Issue #488)

Rotates the index-encryption key, re-encrypting every persisted index file from
the old key to the new one. All modes require an **encrypted, index-persistent**
database opened via `ALETHEIADB_CONFIG`.

```bash
export ALETHEIADB_CONFIG=/etc/aletheiadb/aletheia.toml

# How far along is a rotation? (on-disk key-generation classification)
aletheia keys rotate --status

# Start a rotation to a new key (file- or env-var-sourced)
aletheia keys rotate --new-key /etc/aletheiadb/new-master.key
aletheia keys rotate --new-env-var ALETHEIADB_MEK_NEW

# Finish an interrupted rotation (idempotent) / roll one back
aletheia keys rotate --resume
aletheia keys rotate --cancel
```

A successful start prints an old→new key-version summary and per-file counts;
progress is written to **stderr** so it can be separated from the report.

> **Important — cross-layer refusal.** The shipped engine performs an
> *index-only* rotation and **safely refuses** while any *other* at-rest layer
> (WAL, cold storage, checkpoint) is encrypted under the same master key —
> rotating the index alone would strand those layers. Because AletheiaDB
> encrypts **uniformly** (enabling encryption encrypts the WAL too), a normally
> configured encrypted database has an encrypted WAL, so `keys rotate --new-key`
> will correctly report:
>
> ```
> error: ... refusing: other encrypted-at-rest layers (wal) are present ...
> ```
>
> Full-MEK (all-layer) rotation, which re-keys the WAL/cold/checkpoint too, is a
> documented follow-up.

### `encryption` — at-rest status & verification

```bash
export ALETHEIADB_CONFIG=/etc/aletheiadb/aletheia.toml

# Per-layer status table (WAL / index / checkpoints / cold)
aletheia encryption status

# Prove the configured cipher actually DECRYPTS the data at rest: opens the
# database, replays the (encrypted) WAL, loads the (encrypted) index files, and
# classifies them through the live keyring. A wrong/missing key fails the open.
aletheia encryption verify
```

`encryption verify` exits `0` with `encryption verify: PASS` when the data
decrypts, and non-zero with a `FAILED` message (no key bytes) otherwise.

### `encryption enable` / `disable` — not yet supported

In-place migration of a database **between** plaintext and encrypted-at-rest is
**not implemented**. It requires a full-database migration engine that
re-encrypts every WAL segment, checkpoint, index file, and cold-storage entry
crash-consistently — distinct from the `keys rotate` engine, which only re-keys
*already-encrypted* index files between generations. These subcommands therefore
return a specific, non-zero error rather than faking success:

```bash
aletheia encryption enable    # error: ... requires a full-database migration engine ...
```

To use encryption at rest, create the database with encryption enabled in its
configuration **from the start** (`[encryption] enabled = true`).
