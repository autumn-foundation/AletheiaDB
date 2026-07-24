# ADR-0059: Asynchronous WAL-Shipping Replication

**Status:** Accepted
**Date:** 2026-07-23
**Deciders:** AletheiaDB Core Team
**Categories:** replication, durability, availability, api, network
**Issue:** #3355

## Context

AletheiaDB runs as a single process: one node holds the only live copy of
the data, and all reads contend with writes on the same instance. There is
no way to scale reads across machines, and no warm standby to fail over to
if the process or its host is lost. The WAL already produces a totally
ordered, durable stream of every committed write — the natural shipping
unit for replication — but nothing delivered it to a second process.

This ADR covers the **asynchronous slice** of SPEC-011
(`docs/specs/011_distributed_replication.md`): pull-based WAL shipping,
strictly read-only replicas, lag observability, manual promotion, and
failure handling. Synchronous/quorum replication, automatic
election/failover, multi-primary, and cross-shard replication are
explicitly out of scope for this decision (see "Alternatives Considered"
and the design plan's "Out of scope" section,
`docs/plans/2026-07-23-3355-async-replication.md`).

## Decision

### Pull-based WAL shipping over TCP

A replica polls the primary's feed (`ReplicationFeed`,
`src/storage/replication/feed.rs`) for **durable** (flushed-to-segment) WAL
entries starting at its own last-applied LSN. This keeps the primary's
write/commit path completely free of replication-aware code — the feed only
ever reads segment files a background connection asks for — and requires no
new async runtime or HTTP dependency (`std::net::TcpStream` only). The wire
protocol (`src/storage/replication/tcp.rs`) is a length-prefixed framed
protocol: control messages as `serde_json`, WAL entries reusing the exact
on-disk WAL entry codec so there is no second serialization format to keep
in sync.

### Strictly read-only replicas

A replica's role is an always-compiled atomic (`NodeRole`,
`src/db/replication_role.rs`) checked at every write-transaction
construction seam, the commit-time promotion-race recheck, the single MCP
dispatch seam, and the single HTTP dispatch seam. A write/admin operation
against a replica is rejected with a structured, non-retriable
`FAILED_PRECONDITION` (`TransactionError::ReadOnlyReplica`, rendered
identically by both surfaces as
`{"node_role": "replica", "reason": "read_only_replica"}`). This enforcement
is always compiled — a standalone database is simply "the primary with zero
replicas" and pays no cost for it.

### Whole-commit-frame apply, never a torn transaction

The replica applier accumulates fetched entries across polls and only
advances its applied position after resolving a complete
`[BeginTx .. CommitTx]` band (mirroring
`crate::storage::recovery::resolve_transaction_frames`'s own acceptance
checks), applying through the same recovery replay engine used by crash
recovery and point-in-time restore. A trailing incomplete band is treated as
"not yet arrived," not "never happened" — the opposite interpretation from
crash recovery, and the reason this is a distinct wrapper
(`src/storage/replication/apply.rs`) rather than a direct call into the
recovery module.

### Manual promotion only

`AletheiaDB::promote_to_primary()` stops the applier, seeds the local WAL's
next LSN to `applied_lsn + 1` (so a newly-accepted write's LSN can never
collide with already-applied history), persists indexes at the promotion
point, and flips the role atomic — all local, in-process work with no
network round-trip to the (by-definition unreachable, in a real failover)
old primary. Exposed as `POST /admin/promote` (Admin-class, the one write
path a replica is deliberately allowed to accept) for server deployments and
directly in the Rust API for embedded use. There is no automatic election or
fencing; the operator decides when and what to promote and is responsible
for isolating the old primary first (see the
[Promotion Runbook](../guides/promotion-runbook.md)).

### Token-authenticated TCP, no encryption in v1

Every connection performs a handshake verified with a constant-time SHA-256
digest comparison (mirroring the existing API-key verification), and the
plaintext token is never retained past hashing or logged. The wire protocol
itself carries no transport encryption in v1 — operators must run
replication over a trusted network segment.

### Snapshot bootstrap via `.albk`

A fresh replica's initial sync (`AletheiaDB::bootstrap_replica`) fetches a
point-in-time snapshot from the primary using the existing `.albk` backup
artifact format (`AletheiaDB::backup`) rather than inventing a second
snapshot format, then resumes streaming from exactly the snapshot's recorded
`source_lsn`. Replica durability during ongoing streaming is likewise
achieved by reusing existing machinery: the applier periodically persists
**index snapshots stamped with the primary's LSN** (the same manifest
mechanism `.albk` restore uses), rather than having the replica re-append
entries to its own local WAL — local LSN allocation would diverge from the
primary's LSN space and corrupt the resume position.

## Alternatives Considered

- **Synchronous replication** (primary blocks on replica ack before
  committing). Rejected for this slice: it couples the primary's
  write-latency to network round-trips and replica health, directly
  conflicting with the project's <1µs/</10ms current-state and temporal
  query targets on the write path. Async keeps the primary's write path at
  effectively zero replication overhead by construction.
- **Push-based streaming from the group-commit hook.** Considered for a
  lower lag ceiling, but rejected for this slice: it couples the commit path
  itself to the network and needs a backpressure design the pull-based
  model avoids entirely (a slow/disconnected replica cannot affect the
  primary at all in the pull model).
- **Logical/statement-based replication** (ship high-level operations
  rather than WAL bytes). Rejected: the WAL is already the durable,
  totally-ordered source of truth for every committed write, framed into
  atomic commit bands (#3413) — re-deriving a second logical representation
  of the same facts would be redundant machinery with its own
  correctness surface, for no benefit over shipping the WAL directly.
- **HTTP transport riding the existing `http-server`.** Rejected: it would
  drag `tokio` and `reqwest` into the replication path and couple
  replication availability to the HTTP feature; a self-contained
  `std::net::TcpStream` protocol has no such dependency and no such
  coupling.
- **External broker (Kafka-style).** Explicitly rejected by the originating
  issue ("no external broker") — AletheiaDB's replication has no operational
  dependency beyond the two database processes and the network between
  them.
- **Filesystem/segment-shipping (rsync-style).** Rejected as the primary
  mechanism: it cannot express "connect + initial sync + resume" with
  bounded lag for a remote replica the way a live pull protocol can. The
  `ReplicationSource` trait deliberately keeps this door open for a future
  transport without touching anything above it.
- **Consensus-based automatic failover (Raft/similar).** Explicitly deferred,
  out of scope for this slice. Manual promotion with an operator-driven
  runbook is the entire failover story in v1; automatic election, fencing,
  and multi-primary topologies are a distinct, much larger design surface
  left for a future ADR.

## Consequences

- **RPO is greater than zero by design.** Because replication is
  asynchronous, any primary write not yet applied to the promoted replica at
  the moment of a primary loss is lost. This is bounded and observable
  (`entries_behind`/`lag_ms` via `replication_progress()`/`database_stats`),
  but it is never zero — operators must size their tolerance for this
  explicitly, and it is the direct tradeoff for keeping the primary's write
  path free of replication overhead.
- **Failover is entirely operator-driven.** There is no automatic detection,
  election, or fencing. A real incident requires a human (or external
  tooling) to detect the failure, fence the old primary, choose and inspect
  a promotion candidate, and promote it, following the
  [Promotion Runbook](../guides/promotion-runbook.md). Split-brain is a real
  risk if fencing is skipped or delayed.
- **No transport encryption in v1.** Replication traffic (including the auth
  handshake's aftermath and all replicated data) is plaintext on the wire.
  Deploying it safely requires a trusted network segment or an
  operator-managed tunnel; this is a real operational constraint, not merely
  a documentation footnote.
- **WAL retention is now coupled to replica health.** A replica that falls
  behind further than the primary's retained WAL segments cannot resume — it
  must be re-bootstrapped from a fresh snapshot (`resync_required`).
  Operators must size WAL retention against the longest tolerable replica
  outage, or accept that a long-disconnected replica will need re-bootstrap.
- **New engine surface, but no new on-disk WAL format.** Replication reuses
  the existing WAL entry codec, commit-band framing, and recovery replay
  engine end-to-end; no temporal-invariant or storage-format change was
  needed to support it, and no feature-flag cohort gates it (the engine is
  compiled on every native target, absent only on the ephemeral `wasm32`
  profile, alongside the rest of the durability stack it builds on).
- **Sets up, but does not implement, future work.** The `ReplicationSource`
  trait's transport-agnostic design leaves room for a future transport
  (e.g., TLS-wrapped or a different protocol) without touching the applier
  or feed logic above it; automatic failover/consensus and an incremental
  (rather than full-rebuild) temporal-index apply path are both explicitly
  deferred follow-ups, not implied by this decision.
