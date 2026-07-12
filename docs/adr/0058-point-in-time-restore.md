# ADR-0058: Point-in-Time Restore (PITR) to a Transaction-Time Coordinate

**Status:** Accepted
**Date:** 2026-07-12
**Deciders:** AletheiaDB Core Team
**Categories:** durability, recovery, backup, api, cli
**Issue:** #3374

## Context

Backup/restore (#3217, `.albk`) recovers a database to the exact moment a
backup was taken. It does not answer the incident that actually pages
operators: *"a bad batch job / compromised credential / runaway agent wrote
garbage for the last 40 minutes — put the database back to 14:05, before it
started."* Without point-in-time restore (PITR) the only options are restore to
the last backup (losing every good write since) or hand-edit live data.

AletheiaDB's WAL already records everything needed: a total order over
transactions, each committed transaction framed as one atomic
`[BeginTx .. CommitTx]` band carrying an authoritative commit timestamp
(#3413), and a deterministic crash-recovery replay
(`replay_entries_into_storage_with_constraints`). PITR is a **product surface
over guarantees that already exist**: replay the archived WAL over a base
backup, but *stop* at an operator-chosen transaction-time coordinate.

Note the distinction from bi-temporal reads: `AS OF` *views* the past while
history is intact; PITR is for when recorded history after a point is
*unwanted* (bulk corruption, security incident, compliance rollback) and the
operator needs a running database whose recorded state ends at that point — a
physically restored instance, not a read-time lens.

## Decision

Add a bounded-replay PITR path in the backup lane
(`src/db/pitr.rs`), plus a structured out-of-window error and CLI/dry-run
surfaces.

### Target model

```rust
pub enum PitrTarget { AsOf(Timestamp), Lsn(u64) }
```

Transaction time only. Valid-time-targeted restore is a category error
(valid time is a query dimension) and is out of scope. The target resolves
**inclusively at-or-before** the coordinate: every transaction committed
at-or-before the target is present, every one after it is absent. A target
between two transactions lands on the earlier one.

### Band-boundary filter

`filter_bands` groups the LSN-sorted WAL stream into whole transaction
**bands** (`[BeginTx .. CommitTx]` frames; legacy unframed entries and
self-committing control ops are singleton bands) and returns the prefix of
whole bands whose stop coordinate is at-or-before the target. It **never**
emits a partial band and **drops an incomplete trailing band** (a torn crash
tail). It also returns the applied/discarded transaction counts and the
resolved stop coordinate, which feed the dry-run plan. This function is pure
and unit-tested against exact-boundary, between-transactions, before-all,
after-all, torn-tail, and mixed framed/legacy streams.

### Restore mirrors startup differential replay

`restore_to_data_dir_at(albk, wal_archive, target, data_dir)`:

1. `check_target_empty` (same atomicity posture as #3217).
2. `read_artifact` → `source_lsn`; read the archived WAL; compute the window
   and validate the target **before** touching the target directory.
3. `materialize_to_dir` the base snapshot and open the base-state database
   through `durable_config_for_data_dir` — reusing the tested startup path,
   which finalizes the base (id generators, temporal index, extent, HLC seed)
   from the snapshot at `source_lsn`.
4. Apply the **net constraint declarations** from the full archive up to the
   target (declarations may predate the backup and are not carried by the base
   snapshot — mirroring the startup constraint pass over the whole WAL).
5. Replay the **band prefix** (post-`source_lsn`, at-or-before target) into
   current + historical storage via
   `replay_entries_into_storage_with_constraints`, then finalize exactly as
   startup does: seed id generators and `next_version_id`, rebuild the
   reservation index, rebuild the temporal index, reseed the HLC.
6. Record the net constraint declarations in the target WAL (index snapshots do
   not carry constraints) and `persist_all_indexes` so a subsequent reopen
   loads the target state.

Choosing `source_lsn` as the replay floor (never below it) means the base
snapshot's already-captured version is never double-applied, and the idempotent
re-application guards (#3419) in the replay make the boundary safe regardless.

### Inputs are read-only; the original data dir is untouched

PITR never writes to the `.albk` or the WAL archive, and always produces a
**fresh** directory (no in-place rollback). The recommended operational flow is
side-by-side restore-then-switch.

### Dry-run / window inspection

`inspect_pitr(albk, wal_archive, target)` returns a serde-serializable
`PitrPlan { earliest, latest, resolved_stop, transactions_applied,
transactions_discarded }` without materializing or opening anything, so an
operator can assess blast radius before acting.

### Achievable window and out-of-window error

The window is bounded **below** by the base backup (PITR cannot reconstruct a
coordinate before `source_lsn` from base + forward replay) and **above** by the
archived WAL tail. A target above the tail is not an error — it resolves to a
full replay ("everything at-or-before the target"). A target **below** the
window fails with a structured

```rust
BackupError::TargetOutsideWindow { requested, earliest, latest }
```

mapped at the MCP boundary (`src/mcp/error.rs`) to `FAILED_PRECONDITION`
(non-retriable), mirroring the #3234 structured-error contract; the message
names the window.

Above-window rejection is deliberate (F2): an explicit target past the tail
almost always means the operator misjudged the retained window, and silently
full-replaying would hide that. The legitimate "restore to the tail" intent is
expressed by supplying **no target** (the CLI's bare `--wal-archive`, or the
explicit `--latest` alias), which resolves to the latest reachable coordinate
(itself in-window) and performs a full replay. In-window target → partial
restore; no target / `--latest` → full replay to the tail; above-window explicit
target → error.

### Interner vocabulary-change guard

The WAL stores node/edge labels and property keys as **raw `u32` interner ids**,
and the base `.albk` only carries the interner as of `source_lsn`. A post-backup
transaction that introduces a **brand-new label or property key** has an id
`>= K` (the restored interner's string count). Replaying that id verbatim is
**silent data corruption**, not a mere failed lookup: it first dangles, then —
because the restored interner's `next_id` equals `K` — the first genuinely-new
string a later write interns collides with the dangling id, so a replayed
node/edge is **mislabeled** or its property dropped.

PITR therefore scans the included band prefix and the constraint-declaration
slice for any interner id `>= K` **before materializing anything** and, if found,
fails with a structured

```rust
BackupError::WindowCrossesVocabularyChange { first_unresolved_id, restored_interner_count }
```

also mapped to `FAILED_PRECONDITION`. This converts silent mislabeling/dropping
into a clean, honest failure. The remediation is to take a fresh base backup
that includes the new vocabulary, or to target a coordinate before the change.

## Consequences

- **RDBMS-grade DR.** PITR to any coordinate in the retained window with
  bounded, known data loss (only the operator-chosen suffix), matching the
  operational trust bar of PostgreSQL/MySQL and leapfrogging snapshot-only graph
  incumbents.
- **Operator-managed WAL archiving (v1).** PITR reaches only as far as the
  archived WAL chain + base backup allow. Retaining/archiving WAL segments
  (rather than truncating after checkpoint/cold-migration) is an
  operator-managed prerequisite in v1; an integrated retention policy is a
  follow-up.
- **Interner vocabulary caveat (guarded, not silent).** The WAL stores labels
  and property keys as interner ids (property *values* are self-contained). The
  base backup carries the interner as of `source_lsn`, so a post-backup
  transaction introducing a brand-new label or property key cannot be resolved
  after replay — and replaying it verbatim would **silently mislabel or drop
  data**, not merely fail to resolve. PITR detects this before materializing and
  fails cleanly with `WindowCrossesVocabularyChange` (see above). Keeping the
  label/key vocabulary stable across the window is required in v1; a durable
  interner archive is a follow-up.
- **Cold-tier caveat.** As with `temporal_extent` (#3238), a restored
  instance's cold-tier coverage depends on the base backup; the window bounds
  are stated in the same terms.
- **No new on-disk format.** PITR is pure replay machinery over the existing
  `.albk` artifact and WAL segment format; no temporal-invariant change (replay
  determinism is the guarantee being productized) and no feature-flag cohort.

## Alternatives Considered

- **Re-serialize a truncated WAL and let plain startup replay it.** Rejected:
  byte-precise segment truncation/re-encoding is fragile and would touch the
  WAL lane; filtering decoded entries and feeding the existing replay is simpler
  and reuses tested machinery.
- **Error on targets above the archived tail.** Rejected: the issue's
  at-or-before tie-break makes "target after all" a well-defined full replay,
  not an error.
