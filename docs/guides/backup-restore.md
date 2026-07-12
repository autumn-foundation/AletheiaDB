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

# Point-in-time restore (Issue #3374): replay an archived WAL over the base
# to a target transaction-time coordinate (see "Point-in-Time Restore" below).
aletheia restore <input_path> --wal-archive <dir> [--as-of <iso8601|micros> | --lsn <n>] [--dry-run]
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

## Point-in-Time Restore (PITR)

Plain restore recovers a database to the moment a `.albk` backup was taken. **Point-in-time
restore** (Issue #3374) reaches *any* transaction-time coordinate between the backup and the
present, by replaying an **archived WAL chain** over the base backup and **stopping** at an
operator-chosen target. Use it when recorded history after a point is *unwanted* — a bad batch
job, a compromised credential, a runaway agent, a compliance-mandated rollback of a bad
ingestion — and you need a running database whose recorded state ends at the last known-good
moment, with bounded, known data loss (only the unwanted suffix).

> PITR targets **transaction time only**. Restoring "to a valid time" is a category error:
> valid time is a query dimension (`AS OF VALID_TIME`), not a physical recovery coordinate.
> When history itself is intact, use bi-temporal `AS OF` reads instead — PITR is for when the
> recorded tail is *unwanted*, not merely *old*.

### Prerequisite: an operator-managed WAL archive

PITR reaches only as far as the **base backup + archived WAL chain** allow. The WAL archive is a
directory of the WAL segment files (`*.log`) covering the window you want to restore into. In
v1 this archive is **operator-managed**: you must retain/copy WAL segments rather than letting
them be truncated after checkpoint or cold-tier migration.

Recommended retention runbook:

1. Take periodic base backups: `aletheia backup /backups/base-$(date +%s).albk`.
2. Continuously archive WAL segments to durable storage (e.g. `rsync`/object-store sync of the
   `wal/` directory). Keep segments for at least your target RPO window plus one backup interval.
3. Do **not** delete archived segments that are newer than your oldest retained base backup —
   they are the replay chain PITR depends on.

RPO statement: with this configuration, the achievable RPO to any coordinate in the retention
window is **0 discarded good transactions** — the loss is exactly and only the operator-chosen
suffix.

### Inspecting the window (`--dry-run`)

Before restoring, inspect the achievable window and the blast radius of a target — without
materializing or opening anything:

```text
aletheia restore /backups/base.albk --wal-archive /archive/wal --dry-run
aletheia restore /backups/base.albk --wal-archive /archive/wal --as-of 2026-07-12T14:05:00Z --dry-run
```

The dry-run prints a `PitrPlan` as JSON:

```json
{
  "earliest": { "lsn": 98765, "timestamp_micros": 1783840000000000, "timestamp_rfc3339": "2026-07-12T13:59:00.000000Z" },
  "latest":   { "lsn": 120400, "timestamp_micros": 1783843200000000, "timestamp_rfc3339": "2026-07-12T14:40:00.000000Z" },
  "resolved_stop": { "lsn": 110230, "timestamp_micros": 1783840300000000, "timestamp_rfc3339": "2026-07-12T14:05:00.000000Z" },
  "transactions_applied": 842,
  "transactions_discarded": 205
}
```

`transactions_discarded` is the rollback blast radius: the count of committed transactions after
the target that this restore would drop.

### Performing the restore

```text
# Stop at a wall-clock coordinate (ISO 8601 / RFC 3339):
ALETHEIADB_DATA_DIR=/restore/side-by-side \
  aletheia restore /backups/base.albk --wal-archive /archive/wal --as-of 2026-07-12T14:05:00Z

# ...or at an exact WAL LSN (or microseconds-since-epoch for --as-of):
ALETHEIADB_DATA_DIR=/restore/side-by-side \
  aletheia restore /backups/base.albk --wal-archive /archive/wal --lsn 110230
```

`--as-of` and `--lsn` are **mutually exclusive**; pass exactly one. The target directory
(`ALETHEIADB_DATA_DIR`) must be empty, matching plain restore's atomicity posture. The produced
directory reopens through the canonical `AletheiaDB::open(data_dir)` path with the target state
intact.

Rust API:

```rust
use aletheiadb::{AletheiaDB, PitrTarget, Timestamp};

// Dry-run: inspect the window + blast radius.
let plan = AletheiaDB::inspect_pitr(&albk, &wal_archive, Some(PitrTarget::AsOf(target_ts)))?;

// Restore to a fresh, empty data dir.
let db = AletheiaDB::restore_to_data_dir_at(
    &albk, &wal_archive, PitrTarget::AsOf(target_ts), &data_dir,
)?;
```

### Band-boundary stop semantics

Every committed transaction is one atomic `[BeginTx .. CommitTx]` band in the WAL. PITR includes
the prefix of **whole bands** committed **at-or-before** the target and never a partial band. A
target that falls **between** two transactions lands on the earlier one (inclusive tie-break):
everything committed at-or-before the coordinate is present, everything after it is absent. An
incomplete trailing band (a crash-torn tail) is dropped. A target **above** the archived tail
resolves to a full replay ("everything at-or-before the target"); a target **below** the base
backup fails (see next).

### Target outside the window

The achievable window is bounded **below** by the base backup — PITR cannot reconstruct a
coordinate before the backup from base + forward replay — and **above** by the archived WAL tail.
A target below the window fails with a structured `BackupError::TargetOutsideWindow` naming the
window (`earliest`/`latest`); on the MCP surface this maps to `FAILED_PRECONDITION`. Run
`--dry-run` first to pick a reachable coordinate.

### Side-by-side restore-then-switch flow

PITR always produces a **fresh** directory and never mutates the base backup, the WAL archive, or
your original (pre-incident) data directory. The recommended incident flow:

1. **Assess** — `--dry-run` against candidate targets to bound the blast radius.
2. **Restore side-by-side** — restore into a *new* empty directory (leave production untouched).
3. **Verify** — open the restored directory, run integrity/temporal spot checks, confirm the bad
   writes are gone and good writes are present.
4. **Switch** — cut traffic over to the restored instance (or promote its data directory).
5. **Retain** — keep the original directory until the switch is confirmed good.

### Constraints, provenance, and cold tier

- **Constraints (#3218)** declared before the target are re-established and enforced in the
  restored instance (including declarations that predate the base backup), and survive a reopen.
- **Provenance (#3224)** on replayed versions is preserved through PITR.
- **Cold tier (#3238)** coverage of the restored instance depends on what the base backup
  captured; the window bounds are stated in the same terms as `temporal_extent`.

## Performance Notes

- Backup throughput scales linearly with total version count. A 10K-node /
  50K-edge dataset with full history typically completes in under 5 seconds.
- The artifact is zstd-compressed (level 3 by default), reducing size 60–75%
  relative to uncompressed bitcode.
- Restore from a 10K/50K dataset: under 5 seconds (dominated by zstd
  decompression and index reconstruction).

## Limitations

### Single-process restore constraint

`restore` and `restore_to_data_dir` call `GLOBAL_INTERNER.clear()` to reset the
process-wide string interner before re-loading strings from the backup. This is
safe only when the process contains **exactly one** AletheiaDB instance and restore
runs **before any queries begin**. Calling restore while other live instances share
the same process will invalidate their interned string IDs, causing data corruption.

> **Future work**: making the string interner instance-local will lift this restriction.

### Cold-tier memory usage

During backup, cold-tier historical versions (`scan_node_versions_cold` /
`scan_edge_versions_cold`) are loaded into memory in full before being serialised
into the artifact. For databases with millions of cold-tier versions, peak backup
memory equals the total in-memory size of those version objects. A streaming cold
scan API that avoids this peak is planned as follow-up work.

### Decompression size limit

Restore enforces a 5 GiB cap on decompressed payload size to guard against
decompression-bomb denial-of-service attacks. Artifacts larger than 5 GiB
uncompressed are rejected with `BackupError::Corrupt`.

### Point-in-time restore (v1)

- **WAL archive must be supplied.** PITR reaches only as far as the base backup + archived WAL
  chain allow; there is no PITR from a `.albk` alone.
- **Retention is operator-managed.** v1 has no built-in WAL retention/rotation policy — you must
  archive segments yourself (see the retention runbook above). An integrated policy is a
  follow-up.
- **Band-granularity stop.** The stop coordinate resolves to a whole transaction boundary
  (inclusive at-or-before); PITR never splits a transaction.
- **Interner vocabulary.** The WAL stores node/edge labels and property keys as interner ids
  (property *values* are self-contained). The base backup carries the interner as of
  `source_lsn`, so a post-backup transaction that introduces a **brand-new label or property
  key** cannot be resolved after replay. Keep the label/key vocabulary stable across the window;
  a durable interner archive is a follow-up.
- **Encrypted WAL archives** are not yet supported by the PITR reader (plaintext segments only).
