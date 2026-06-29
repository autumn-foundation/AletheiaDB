# Backup and Restore

AletheiaDB provides a portable backup/restore mechanism that captures the complete
bi-temporal state of a database — current nodes and edges, every version in the
hot/warm tiers, every version in the cold (Redb) tier, and the string interner —
into a single, self-contained artifact file (`*.albk`).

## Quick Start

### Rust API

```rust
use aletheiadb::AletheiaDB;
use std::path::Path;

// --- Backup ---
let db = AletheiaDB::new()?;
// ... write data ...
let summary = db.backup(Path::new("/backups/snapshot.albk"))?;
println!("backed up {} node versions, {} edge versions ({} bytes)",
    summary.node_versions, summary.edge_versions, summary.bytes_written);

// --- Restore (ephemeral, in-memory) ---
let restored = AletheiaDB::restore(Path::new("/backups/snapshot.albk"))?;

// --- Restore (durable, into an empty data directory) ---
AletheiaDB::restore_to_data_dir(
    Path::new("/backups/snapshot.albk"),
    Path::new("/var/lib/myapp/db"),
)?;
```

### CLI

```bash
# Backup the database configured via ALETHEIADB_DATA_DIR
ALETHEIADB_DATA_DIR=/var/lib/myapp/db aletheia backup /backups/snapshot.albk

# Restore into a fresh, empty target directory
ALETHEIADB_DATA_DIR=/var/lib/myapp/db-restored aletheia restore /backups/snapshot.albk
```

## API Reference

### `AletheiaDB::backup(&self, path: &Path) -> Result<BackupSummary>`

Creates a portable backup artifact at `path`.

- The artifact is written **atomically** via a temp-file + rename pattern: an
  interrupted backup never leaves a partial file at `path`.
- A single Arc-COW snapshot is taken at the current WAL LSN so the artifact
  represents a **consistent point in time**. Concurrent writes are either fully
  included (committed before the snapshot) or fully excluded.
- Cold-tier versions are scanned outside the historical read-lock to avoid
  blocking live queries during disk I/O.

`BackupSummary` fields:

| Field | Type | Description |
|---|---|---|
| `node_versions` | `u64` | Total node versions captured (hot + cold) |
| `edge_versions` | `u64` | Total edge versions captured (hot + cold) |
| `current_node_count` | `u64` | Live nodes in the current-state store |
| `current_edge_count` | `u64` | Live edges in the current-state store |
| `bytes_written` | `u64` | Compressed artifact size in bytes |
| `source_lsn` | `u64` | WAL LSN at the time of the snapshot |

### `AletheiaDB::restore(path: &Path) -> Result<AletheiaDB>`

Restores into a fresh **ephemeral** (in-memory) instance. The returned database
behaves identically to one created with `AletheiaDB::new()`. The backing temp
directory is held alive for the lifetime of the returned instance.

### `AletheiaDB::restore_to_data_dir(path: &Path, data_dir: &Path) -> Result<AletheiaDB>`

Restores into a **durable** directory at `data_dir`.

- Returns `Error::Backup(BackupError::TargetNotEmpty)` if `data_dir` already
  contains index files — preventing accidental data overwrites.
- On success the database is persisted to `data_dir` and can be reopened later
  with a durable `AletheiaDBConfig` pointing at that directory.

## Artifact Format

| Offset | Size | Description |
|---|---|---|
| 0 | 4 B | Magic: `ALBK` |
| 4 | 2 B | Format version (little-endian `u16`), currently `1` |
| 6 | N B | Zstd-compressed bitcode payload |

The payload is a `BackupPayload` struct containing:
- `StringInternerData` — the complete string interner table
- `GraphIndexData` — current-state nodes and edges
- `TemporalIndexData` — all node/edge versions with bi-temporal intervals
- Metadata: `created_at_micros`, `source_lsn`, version counts

## Error Types

| Variant | When |
|---|---|
| `BackupError::Io(String)` | File system or I/O failure |
| `BackupError::Serialization(String)` | Bitcode encode/decode failure |
| `BackupError::BadMagic` | File is not a valid `.albk` artifact |
| `BackupError::IncompatibleVersion { found, supported }` | Artifact was written by a newer AletheiaDB version |
| `BackupError::TargetNotEmpty` | `restore_to_data_dir` called on a non-empty directory |
| `BackupError::Corrupt(String)` | Data integrity check failed |

## Consistency and Atomicity Guarantees

- **Write atomicity**: artifact is written to a temp file, fsynced, then renamed.
  A crash mid-backup leaves no partial artifact at the target path.
- **Read consistency**: the backup captures one LSN-anchored snapshot. No
  concurrent write appears partially.
- **Restore atomicity**: data is materialized into a temp directory first; only
  once fully written is the DB opened from it. A crash mid-restore leaves `data_dir`
  untouched (for `restore_to_data_dir`).

## Tier Independence

Cold-tier versions are folded into the hot set inside the artifact. After restore
the database starts with all versions in the hot tier and migrates them to cold
storage according to the target instance's own tiered-storage policy. This means
a backup taken from a database with cold storage enabled can be restored into an
instance without cold storage (and vice versa) with no version loss.

## Format Version Contract

- The current format version is `1`.
- Artifacts with an unknown (higher) `format_version` are rejected with
  `BackupError::IncompatibleVersion { found, supported: 1 }`.
- Older artifacts (lower version) will be handled with a migration path in future
  releases; currently only version `1` is produced and accepted.

## CLI Integration

```text
aletheia backup  <output_path>   # backup → prints JSON summary
aletheia restore <input_path>    # restore → ALETHEIADB_DATA_DIR must be set and empty
```

JSON output from `aletheia backup`:

```json
{
  "ok": true,
  "bytes_written": 143827,
  "node_versions": 12500,
  "edge_versions": 48200,
  "current_node_count": 10000,
  "current_edge_count": 40000,
  "source_lsn": 98765
}
```

## Performance Notes

- Backup throughput scales linearly with total version count. A 10K-node /
  50K-edge dataset with full history typically completes in under 5 seconds.
- The artifact is zstd-compressed (level 3 by default), reducing size 60–75%
  relative to uncompressed bitcode.
- Restore from a 10K/50K dataset: under 5 seconds (dominated by zstd
  decompression and index reconstruction).
