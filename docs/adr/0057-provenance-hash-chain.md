# ADR-0057: Tamper-Evident Provenance Hash Chain

**Status:** Accepted
**Date:** 2026-07-12
**Deciders:** AletheiaDB Core Team
**Categories:** storage, transaction, security, audit

## Context

AletheiaDB's value proposition is that recorded history is *trustworthy*: an LLM
(or auditor, or regulator) can ask "what did the system know about X, and when?"
and rely on the answer. Bi-temporal versioning already makes history
*queryable* and append-only *by convention*, and the signed audit export
(Issue #3358) lets a third party verify a single entity's history offline. What
was missing is a **database-wide, tamper-evident** guarantee: a way to detect
that stored history was altered after the fact — a version's bytes edited, a
transaction deleted or reordered or inserted, the tail truncated — and, against
an externally held anchor, to *prove* rollback and fork.

The constraints that shaped the design:

- **Zero behavior/format change when disabled.** The overwhelming majority of
  deployments will not enable this. A disabled build must be byte-identical on
  disk and on the hot path.
- **Negligible write-throughput cost when enabled.** The acceptance metric
  (#3383 concern) is ≥ 90% of the chain-disabled GroupCommit baseline.
- **Recoverable.** The guarantee must survive crash recovery: after WAL replay
  the chain must cover the full recovered history.
- **Do not touch the WAL format.** The WAL, recovery, transaction, and
  historical-storage layers are load-bearing and out of scope for churn
  (durable rehydration of chain state is a tracked follow-up, mirroring the
  lineage index in Issue #3371).

## Decision

Introduce an **opt-in, self-contained sidecar** (`src/provenance_chain/`) that
binds every committed transaction into a domain-separated SHA-256 hash chain
over the database's recorded history.

### Chain construction

The chain unit is a **committed transaction**. The versions produced by one
transaction share one commit timestamp, which is the grouping and ordering key.
The fold is three levels of domain-separated SHA-256:

1. **Leaf** — each version is normalized to a canonical, injective
   `VersionHashInput` and hashed to a `version_leaf`. Exact leaf coverage (what
   a byte-edit of the stored version must not change to pass verify): entity
   id + kind, version id and `prev_version_id`, node **label**, edge
   **source/target**, `valid_from` and `transaction_from`, provenance, the
   **sorted** property set, the `is_tombstone` flag, and — for a *born-closed
   terminal* version (a delete tombstone or a retraction) — its `valid_to`. The
   interval **ends** of a still-live (superseded-later) version are deliberately
   **not** hashed directly (a later supersession mutates them, so they cannot be
   bound into an append-only leaf); they are instead protected by the per-entity
   timeline-consistency check in `verify_full` (monotonic transaction starts +
   per-version interval well-formedness). See the "born-closed" limitation
   below.
2. **Transaction digest** — the leaves of one transaction fold, with the commit
   timestamp and tx id, into a `tx_digest`.
3. **Chain step** — each transaction digest chains onto the previous record's
   digest (`chain_step(prev, tx_digest)`), seeded by an explicit **genesis**
   record (enable LSN + timestamp).

Records are persisted to an append-only log (header + length-framed records +
a head checkpoint), tolerant of a torn trailing record. Verification is
database-independent: it re-fetches each referenced version through a
`VersionSource`, recomputes leaves and the fold, and compares against the sealed
digests — localizing the **earliest** broken sequence number on tamper.

### Durability model: async sealer, re-derivable on recovery

Sealing is **off the commit critical path**. On commit the hot path only
*captures* the version refs the transaction produced (from the write buffer)
and enqueues a `PendingTx` to a background sealer thread that does the hashing
and log append. This is what keeps the enabled path within the throughput
budget.

Because the chain is a pure function of recorded history, it is
**re-derivable on recovery**: at startup, after WAL replay restores history and
before the sealer starts, `rebuild_chain_tail` groups every restored version by
its exact commit timestamp and synchronously seals every transaction beyond the
loaded head. The chain therefore always covers the full recovered prefix
without the WAL carrying any chain-specific payload.

### Per-WAL-mode durability semantics (AC6)

The chain's sidecar-flush policy (`ChainFsyncMode`: `PerTransaction`,
`Batched` (default), `Never`) composes with, but is **independent of**, the WAL
durability mode (`Synchronous`, `GroupCommit`, `Async`). The invariant that
makes every combination safe: **the WAL is the source of truth; the chain is
derived.** A chain record can never be *more* durable than the WAL transaction
it describes, and never needs to be, because on recovery the unsealed tail is
re-derived from replayed history (`rebuild_chain_tail`).

What "durable" means therefore splits along two axes:

1. **Is the transaction itself durable?** Governed entirely by the WAL mode.
   - `Synchronous` — the commit fsyncs the WAL before returning; the transaction
     survives a crash.
   - `GroupCommit` — the commit returns after its batch's fsync completes; the
     transaction survives a crash (ACID, just batched).
   - `Async` — the commit returns before the WAL fsync; a crash can lose the
     most recent transactions (eventual durability). Anything the WAL loses was
     never committed, so the chain correctly never sealed it either.

2. **Is the chain record durable, or re-derived on recovery?** Governed by
   `ChainFsyncMode`, but the answer never affects *correctness* — only whether a
   given record is read back from `chain.log` or re-folded from history:

| WAL mode | `ChainFsyncMode` | Guaranteed sealed-and-durable on the sidecar | Re-derived on recovery |
|----------|------------------|----------------------------------------------|------------------------|
| `Synchronous` / `GroupCommit` | `PerTransaction` | Every transaction whose seal `append` returned before the crash (sealing is async, so the newest *sealed* records are `fsync`-durable, but transactions committed-yet-not-yet-sealed are not on the sidecar). | Any committed transaction the async sealer had not yet processed + fsynced. |
| `Synchronous` / `GroupCommit` | `Batched` (default) | Only records flushed by an explicit `flush()` / shutdown checkpoint or OS writeback. | Everything since the last sidecar flush. |
| `Synchronous` / `GroupCommit` | `Never` | Nothing is guaranteed by the chain layer (OS decides). | Potentially the entire unsealed/unflushed tail. |
| `Async` | any | Same as above, **bounded by** what the WAL retained: a transaction the WAL lost on an async crash is (correctly) neither in history nor in the chain. | The tail the WAL *did* retain but the sealer had not durably sealed. |

The recovery contract is identical in every cell: after WAL replay restores
history, `rebuild_chain_tail` groups every restored version by its exact commit
timestamp and seals each transaction beyond the loaded head, in canonical
commit order. Because the fold is a deterministic function of the
commit-timestamp-ordered record set (finding 4), the re-derived tail reproduces
the exact digests a live seal would have produced — so a pre-crash exported
anchor still verifies as an append-only extension regardless of which
combination above was in effect (test:
`finding4_partial_tail_rebuild_matches_precrash_anchor`).

**Backpressure / tail-latency tradeoff.** Sealing is off the commit critical
path via a bounded channel (`SEAL_CHANNEL_CAPACITY`). When that channel is full
the committing thread **inline-seals** the transaction itself
(`enqueue_commit` → `seal_one`) rather than blocking or dropping it. This
guarantees every committed transaction is sealed exactly once, at the cost of a
tail-latency spike on the commit path under sustained bursts that outrun the
sealer. It is a deliberate drop-to-sync-seal choice: correctness over smooth
latency under overload.

## Alternatives Considered

- **WAL-embedded chain (per-entry digest in the WAL record).** Strongest
  binding, but requires a WAL format change and touches recovery — explicitly
  out of scope, and it would couple the tamper-evidence guarantee to WAL
  framing forever. Rejected in favor of the derivable sidecar.
- **Merkle tree over versions (inclusion proofs).** A Merkle accumulator would
  give O(log n) inclusion proofs and true per-entity Merkle membership. But the
  natural, cheap-to-maintain unit here is the *transaction* (its versions
  already share a commit stamp), and a linear per-transaction chain gives
  precise earliest-tamper localization for free. Entity-scoped verification is
  implemented as a **layered** recomputation over the records that touch the
  entity, not a Merkle-inclusion proof — a deliberate v1 simplification
  (Merkle accumulation is a possible follow-up).
- **Sidecar over historical storage (chosen).** Derives cleanly from recorded
  history, needs no WAL/recovery change, and is fully opt-in. The cost is that
  chain state is currently in the sidecar log only (durable rehydration across
  restart is re-derived from history rather than loaded, as above).

### Commit-timestamp grouping / ordering

Transactions are ordered by their full commit `Timestamp` (`(wallclock,
logical)`), so two transactions sharing a wallclock but differing in the HLC
logical component stay distinct and deterministically ordered. Rebuilt records
use a synthetic, deterministic tx id (the WAL does not preserve the original tx
id for replayed history); verification recomputes purely from stored record
fields, so internal consistency holds regardless.

## Threat Model and the External-Anchor Boundary

**In scope (detectable):** post-hoc mutation of any stored version's content;
deletion, reordering, or insertion of a transaction; truncation of the tail.
Full verification localizes the earliest affected sequence number.

**Requires an external anchor (provable):** an adversary who can rewrite the
*entire* sidecar log can produce a self-consistent alternate chain. Detecting
**rollback** (truncation to an earlier state) and **fork** (divergence)
therefore requires periodically exporting the chain head
(`export_chain_head`) and storing it **offsite**; `verify_chain_against` then
proves the current chain append-only-extends that anchor. The security boundary
is exactly the point at which an anchor was last externalized: anything after
the last exported head is only tamper-*evident* within the log, not
tamper-*proof* against a full-log rewrite.

**The unsealed-tail / offline-tamper boundary (security boundary).**
Tamper-evidence covers the **sealed prefix** only. A version's content is bound
into the chain by its *first seal*. Content altered **before** that first seal —
or altered while the process is offline, before the startup rebuild re-folds the
tail — is re-blessed on rebuild: `rebuild_chain_tail` reads whatever bytes are
in history and seals *those*, so a mutation that lands before sealing becomes
the sealed truth and is not flagged. In practice sealing is near-immediate after
commit (async sealer + inline-seal fallback), so the exposure window is small,
but it is a real boundary: the guarantee is "no post-seal mutation goes
undetected," not "no mutation is ever possible."

**Out of scope (v1 limitations):**

- The exported anchor is **unsigned**. Establishing cryptographic authorship of
  an anchor is left to the operator (e.g. store it in an append-only external
  system, or sign it with the audit key). Signing the anchor is a follow-up.
- Entity-scoped verification is **layered, not a Merkle-inclusion proof** (see
  Alternatives).
- The chain is **not woven into the WAL format**; chain state is lineage-style,
  re-derived on recovery rather than loaded from a durable chain-specific WAL
  payload.
- **Born-closed `valid_to` predicate false-positive.** The leaf binds a
  terminal `valid_to` only when it looks born-closed (`valid_to <=
  transaction_from`, or the tombstone flag). A *heavily backdated* supersession
  can make a legitimately superseded (open-at-seal) version match that predicate
  at verify and produce a **false positive** — a spurious failure, never a
  missed tamper. A stored born-closed discriminator is a follow-up.
- **Cold-tier rebuild.** Verification `fetch` is cold-aware (it reads through
  the tiered hot+cold path, so verify does not false-fail after a sealed version
  migrates to the Redb cold tier). But the startup **crash-rebuild scan**
  (`rebuild_chain_tail`) enumerates **hot-tier history only**; a version
  migrated to cold *before its transaction was ever sealed* would be omitted
  from a from-scratch rebuild. Migration normally happens well after sealing, so
  this is a narrow edge and a tracked follow-up.
- **Unbounded in-memory growth (scaling).** The engine keeps the full
  `records: Vec<ChainTxRecord>` and the derived `EntityIndex` **resident in
  RAM**, growing linearly with total transaction count — there is no spill or
  eviction in v1. Additionally, `verify_full` / `verify_against_anchor` /
  `export` snapshot-**clone** the records vector under the `inner` lock (entity
  verify, post task #1, borrows in place and does not clone). Resident memory
  therefore grows with history and each full snapshot transiently doubles the
  records footprint. Bounding/spilling the in-memory chain (and a
  clone-free/streaming verify) is a tracked follow-up.

## Two In-Scope Audit Security Fixes

Landed with the core (commit `7aaec98`) as part of hardening the verification
path:

1. **Bounded framing on decode.** The append-only store's length-prefixed
   counts are validated against the remaining buffer before allocation, so a
   corrupt/hostile length field cannot drive an unbounded pre-allocation
   (decompression-bomb-style DoS) — a decode returns a structured
   `BadFraming` error instead.
2. **Injective canonical encoding.** The version canonicalization is length-
   prefixed and domain-separated at every level (genesis/leaf/tx/node domains),
   so distinct logical inputs cannot collide by field-boundary ambiguity — the
   precondition for the whole chain's soundness.

## Consequences

### Positive

- Database-wide tamper-evidence over recorded history, opt-in, with precise
  earliest-tamper localization.
- Offsite anchor workflow gives provable rollback/fork detection.
- Disabled by default → byte-identical behavior and on-disk layout for existing
  deployments; enabled path stays within the write-throughput budget (async
  sealer).
- Surfaced on every user-facing surface: Rust API, `aletheia verify` CLI, MCP
  `verify_chain` / `export_chain_head` tools, and the `database_stats` chain
  block.

### Negative

- Enabling the chain adds a background sealer thread and a sidecar log to
  manage.
- Full verification is O(history): it re-fetches and re-folds every version.
  Entity-scoped verification avoids the full scan but is layered, not a
  succinct inclusion proof.

### Neutral

- Chain state is re-derived on recovery rather than loaded from a durable
  chain-specific format; durable rehydration is a tracked follow-up that will
  not change the verification contract.
- `aletheia verify` requires the chain to have been enabled for the opened data
  directory (via an `ALETHEIADB_CONFIG` TOML `[chain]` section); opening a data
  dir alone does not enable it.

## References

- Issue #3351 — Tamper-evident provenance hash chain
- Issue #3383 — Write-throughput budget concern
- ADR-0028 — Encryption at rest (adjacent durability/security surface)
- [docs/guides/provenance-hash-chain.md](../guides/provenance-hash-chain.md) —
  user guide
- Issue #3358 — Signed audit export (single-entity, offline-verifiable)
