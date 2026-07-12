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
   `VersionHashInput` (entity kind/id, version id, bi-temporal bounds,
   provenance, sorted properties) and hashed to a `version_leaf`.
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
`Batched` (default), `Never`) composes with, but is independent of, the WAL
durability mode. The invariant that makes this safe: **the WAL is the source of
truth; the chain is derived.** If the process crashes with sealed WAL
transactions whose chain records were not yet flushed, recovery re-derives them
from replayed history. A chain record can never be *more* durable than the WAL
transaction it describes, and never needs to be — so no durability mode can
produce a chain that disagrees with recovered history.

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

**Out of scope (v1 limitations):**

- The exported anchor is **unsigned**. Establishing cryptographic authorship of
  an anchor is left to the operator (e.g. store it in an append-only external
  system, or sign it with the audit key). Signing the anchor is a follow-up.
- Entity-scoped verification is **layered, not a Merkle-inclusion proof** (see
  Alternatives).
- The chain is **not woven into the WAL format**; chain state is lineage-style,
  re-derived on recovery rather than loaded from a durable chain-specific WAL
  payload.

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
