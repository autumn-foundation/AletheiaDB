# Tamper-Evident Provenance Hash Chain (Issue #3351)

AletheiaDB records bi-temporal history that is append-only *by convention*. The
**provenance hash chain** upgrades that to a **cryptographic, tamper-evident**
guarantee: an opt-in sidecar that binds every committed transaction into a
domain-separated SHA-256 hash chain over recorded history, so that any post-hoc
alteration — editing a byte of a stored version, deleting/reordering/inserting a
transaction, truncating the tail — is **detectable**, and (against an
externally held anchor) rollback and fork are **provable**.

It is **disabled by default**. A database that does not enable it keeps
byte-identical behavior and on-disk layout, and pays nothing on the write path.

> **Scope.** This complements — it does not replace — the single-entity
> [signed audit export](../guides/mcp-query-tool.md) (Issue #3358). The audit
> export proves *one entity's* history offline with a signature; the hash chain
> gives a *database-wide* tamper-evidence guarantee over all recorded history.
> See [ADR-0057](../adr/0057-provenance-hash-chain.md) for the design rationale.

## Enabling the chain

### Programmatically

```rust
use aletheiadb::{AletheiaDB, AletheiaDBConfig};
use aletheiadb::config::WalConfigBuilder;
use aletheiadb::provenance_chain::{ChainConfig, ChainFsyncMode};

let config = AletheiaDBConfig::builder()
    .wal(WalConfigBuilder::new().wal_dir("data/wal").build())
    .chain(ChainConfig {
        enabled: true,
        fsync: ChainFsyncMode::Batched, // default; see durability below
        dir: None,                       // default: <data_dir>/chain
    })
    .build();

let db = AletheiaDB::with_unified_config(config)?;
```

The chain lives under `<data_dir>/chain` by default (the parent of the WAL
directory), or an explicit `dir` override.

### Via TOML (`config-toml`)

The chain round-trips through the unified config file, so the same database can
be opened with the chain enabled by every binary (CLI, MCP server, HTTP server)
that honours `ALETHEIADB_CONFIG`:

```toml
[wal]
wal_dir = "data/wal"

[chain]
enabled = true
fsync = "per_transaction"   # per_transaction | batched | never
# dir = "data/chain"        # optional; defaults to <data_dir>/chain
```

Omitting the `[chain]` section keeps the chain disabled (byte-identical to a
config that predates this feature).

## `aletheia verify` (CLI)

The CLI opens the database via `ALETHEIADB_CONFIG` (TOML) or
`ALETHEIADB_DATA_DIR`, then verifies the chain.

> **The chain must be enabled for the opened data dir.** Opening a data dir
> alone (`ALETHEIADB_DATA_DIR`) does **not** enable the chain. Point
> `ALETHEIADB_CONFIG` at a TOML config whose `[chain]` section has
> `enabled = true`. If the chain is disabled, `aletheia verify` prints a clear
> message and exits non-zero.

```
aletheia verify [--entity <node|edge>:<id>] [--export-head PATH] [--against PATH] [--json]
```

### Full-chain verify (default)

```console
$ ALETHEIADB_CONFIG=aletheia.toml aletheia verify
Provenance chain verification: PASS (scope: full)
  head seq:             128
  head digest:          9f2c…a1
  transactions checked: 128
```

On tamper the earliest broken position is reported and the process exits
non-zero:

```console
$ aletheia verify
Provenance chain verification: FAIL (scope: full)
  head seq:             128
  head digest:          9f2c…a1
  transactions checked: 63
  earliest broken seq:  64
  reason:               recomputed leaf(s) differ from sealed leaves
error: provenance chain verification FAILED (full) at seq 64
```

### Entity-scoped verify

Recompute only one entity's contribution (no full scan):

```console
$ aletheia verify --entity node:42
Provenance chain verification: PASS (scope: entity)
  ...
```

### Export the head anchor

Write the current chain head to a JSON file for offsite storage:

```console
$ aletheia verify --export-head head-2026-07-12.json
Exported chain head anchor to head-2026-07-12.json
  seq:    128
  digest: 9f2c…a1
```

### Verify against a stored anchor (fork/rollback detection)

Prove the current chain append-only-extends a previously exported head:

```console
$ aletheia verify --against head-2026-07-12.json
Provenance chain verification: PASS (scope: anchor)
  ...
```

A missing record at the anchor's sequence is reported as a **rollback**
(truncation); a mismatching digest as a **fork** (divergence). Both exit
non-zero.

Add `--json` to any mode for machine-readable output:

```console
$ aletheia verify --json
{
  "scope": "full",
  "passed": true,
  "head_seq": 128,
  "head_digest": "9f2c...a1",
  "earliest_broken_seq": null,
  "reason": null,
  "transactions_checked": 128
}
```

## MCP tools

Two read-class (`reader` role) tools expose the chain to an LLM/agent:

### `verify_chain`

Three modes, resolved in precedence order:

| Arguments | Mode |
|-----------|------|
| `against` (an exported head object) | **Anchor extension** — prove append-only extension; detect rollback/fork. |
| `entity_kind` (`"node"`/`"edge"`) + `id` | **Entity-scoped** — recompute only that entity's contribution. |
| *(none)* | **Full** — walk the whole chain from genesis; localize `earliest_broken_seq` on tamper. |

Returns `{scope, passed, head_seq, head_digest, earliest_broken_seq, reason,
transactions_checked}`.

### `export_chain_head`

No arguments. Returns the current chain head checkpoint
`{seq, digest, commit_ts, anchor_lsn, genesis_digest}` (digests as lowercase
hex) — store it offsite and pass it back as `verify_chain`'s `against`.

### Errors

Both tools use the [structured error codes](../guides/mcp-query-tool.md)
(Issue #3234). When the chain is not enabled for the database the response is a
non-retriable `FAILED_PRECONDITION` — never a silent empty "pass". A malformed
`against` anchor or a bad `entity_kind` is `INVALID_ARGUMENT`.

## `database_stats` chain block

The `database_stats` tool (and the Rust `AletheiaDB::stats()` snapshot) carries
a `chain` block — an O(1) read of the in-memory head, safe to call frequently:

```json
{
  "chain": {
    "enabled": true,
    "head_seq": 128,
    "head_digest": "9f2c...a1",
    "genesis_digest": "0000...",
    "last_verified": { "passed": true, "at_micros": 1752307200000000 }
  }
}
```

When the chain is disabled, every optional field is `null` and `enabled` is
`false` — never a misleading zero.

## Durability semantics per WAL mode

Sealing runs on a **background sealer thread**, off the commit critical path:
the hot path only captures the version refs a transaction produced and enqueues
them. This keeps enabled-path write throughput within budget (the acceptance
target is ≥ 90% of the chain-disabled GroupCommit baseline; see the
`provenance_chain` benchmark).

The `ChainFsyncMode` flush policy controls how the sidecar log reaches disk,
independently of the WAL durability mode:

| `ChainFsyncMode` | Behavior |
|------------------|----------|
| `PerTransaction` | fsync after every appended chain record (strongest). |
| `Batched` (default) | rely on explicit flush / OS writeback (keeps hashing off the commit path). |
| `Never` | durability is entirely the OS's decision (fastest, weakest). |

The key invariant: **the WAL is the source of truth; the chain is derived.**
The chain is a pure function of recorded history, so it is **re-derivable on
recovery**. At startup, after WAL replay restores history, the unsealed tail is
rebuilt by grouping every restored version by its exact commit timestamp and
sealing each transaction beyond the loaded head. A chain record therefore can
never be *more* durable than the WAL transaction it describes, and never needs
to be: if a crash loses chain records for already-committed WAL transactions,
recovery re-derives them. No WAL durability mode can produce a chain that
disagrees with recovered history.

## External anchoring workflow

Tamper-*evidence* within the log detects mutation, deletion, reordering, and
truncation. Detecting **rollback** and **fork** against an adversary who can
rewrite the *entire* sidecar log requires an external anchor:

1. **Export** the head periodically: `aletheia verify --export-head head.json`
   (or the `export_chain_head` MCP tool).
2. **Store it offsite** — an append-only external system, a signed commit, a
   witness service, etc. (The v1 anchor is unsigned; establishing authorship is
   the operator's responsibility.)
3. **Verify-against** later: `aletheia verify --against head.json` (or
   `verify_chain` with `against`). A truncated chain (rollback) or a diverged
   digest (fork) is reported and exits non-zero.

The security boundary is exactly the last externalized anchor: everything after
it is tamper-evident within the log, but only tamper-*proof* against a full-log
rewrite up to the anchor point.

## Threat boundary (summary)

| Threat | Guarantee |
|--------|-----------|
| Edit a stored version's bytes | Detected by full/entity verify (earliest seq localized). |
| Delete / reorder / insert a transaction | Detected by full verify. |
| Truncate the tail | Detected by verify-against an exported anchor (rollback). |
| Fork to an alternate history | Detected by verify-against an exported anchor (fork). |
| Rewrite the entire sidecar log | Detectable only against an externally held anchor. |

### v1 limitations

- The exported anchor is **unsigned** (authorship is the operator's
  responsibility; signing is a follow-up).
- Entity-scoped verification is **layered**, not a Merkle-inclusion proof.
- The chain is **lineage-style**: it is not woven into the WAL format; chain
  state is re-derived on recovery rather than loaded from a durable
  chain-specific WAL payload (durable rehydration is a tracked follow-up that
  will not change the verification contract).

## See also

- [ADR-0057](../adr/0057-provenance-hash-chain.md) — design rationale and
  alternatives.
- [Access control matrix](../guides/access-control-matrix.md) — `verify_chain`
  / `export_chain_head` are `reader`-class.
- Issue #3358 signed audit export — single-entity, offline-verifiable evidence.
