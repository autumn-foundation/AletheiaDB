# Asynchronous Replication for Read Scale-Out and Failover (Issue #3355)

| Metadata | Details |
| :--- | :--- |
| **Status** | In progress |
| **Issue** | [#3355](https://github.com/autumn-foundation/AletheiaDB/issues/3355) |
| **Spec** | [SPEC-011](../specs/011_distributed_replication.md) (async slice only) |
| **Feature flag** | `replication` (network transport + applier); role enforcement is always-compiled |
| **Complexity** | L |

## Problem

AletheiaDB is a single point of failure: one process holds the only live copy,
and all reads contend with writes on the same instance. The WAL already
produces a totally ordered, durable stream of every write — the natural
shipping unit — but nothing delivers it to a second process. This plan covers
the async slice of SPEC-011: pull-based WAL shipping, strictly read-only
replicas, lag observability, manual promotion, and failure handling.

## Planning

### Brainstorming (candidate architectures)

1. **Pull-based WAL shipping over TCP (chosen)** — replica polls the primary's
   feed for durable (flushed-to-segment) WAL entries from its applied LSN.
   Zero primary write-path coupling (write-overhead metric ≈ 0%), std-only
   networking (no tokio/reqwest dependency), reuses the existing recovery
   replay engine for apply.
2. Push-based streaming from the group-commit hook — lower lag ceiling but
   couples the commit path to the network (write-overhead risk) and needs
   backpressure design. Rejected for v1.
3. HTTP transport riding `http-server` — reuses axum, but drags `tokio` +
   `reqwest` into the replication path and couples replication availability to
   the HTTP feature. Rejected; TCP protocol is self-contained.
4. Filesystem segment shipping (rsync-style) — trivially simple but cannot
   express "connect + initial sync + resume" (AC1/AC6) or serve remote
   replicas with bounded lag. Rejected as the primary mechanism; the
   `ReplicationSource` trait keeps the door open.
5. External broker (Kafka-style, as XTDB) — explicitly rejected by the issue
   ("no external broker").
6. Replica durability via local WAL re-append — rejected: local LSN allocation
   diverges from primary LSNs, corrupting the resume position. Instead the
   replica persists **index snapshots stamped with the primary LSN** (exactly
   how `.albk` restore works: `IndexManifest::new(source_lsn)`), and resumes
   from the manifest LSN after restart.

### Reverse brainstorming ("how would we make this fail?")

- **Ship un-durable entries** → replica applies data the primary loses on
  crash → divergence. Mitigation: the feed reads only flushed segment files
  (`read_from` reads disk, never the ring buffer).
- **Apply a torn transaction** → violated AC3. Mitigation: apply only frames
  resolved by `resolve_transaction_frames` (BeginTx..CommitTx bands, #3413);
  an unterminated tail is "not yet arrived", re-fetched next poll, never
  applied.
- **Advance the applied position before a frame fully applies** → temporal
  queries at ≤ applied position could see partial state. Mitigation: publish
  `applied_lsn` only after the frame's last op is applied.
- **Primary truncates WAL segments the replica still needs** (tiered-storage
  `truncate_to_lsn`, retention sweep) → silent gap. Mitigation: feed compares
  the requested LSN against the minimum available segment LSN and returns a
  structured `ResyncRequired`; the replica surfaces
  `state: "resync_required"` and stops applying (AC6) rather than skipping.
- **Silent write acceptance on a replica** (e.g. a surface not covered by the
  choke-point) → split-brain data. Mitigation: enforce at the Rust
  `write_transaction()` construction seam AND defensively at
  `commit_with_timestamp_inner()`, plus class-based rejection at the single
  MCP dispatch seam and the single HTTP dispatch seam; conformance test sweeps
  every write-class MCP tool and HTTP write variant.
- **Promotion race** — a `WriteTransaction` constructed while still a replica
  commits after promotion (fine: promotion means writable) or a write slips in
  mid-demotion. The role is a single atomic; the commit-time recheck closes
  the construction-time gap.
- **LSN collision after promotion** — new primary allocates LSNs overlapping
  history it applied. Mitigation: promotion seeds the local WAL's next LSN to
  `applied_lsn + 1` before accepting writes.
- **Replica bootstrap from an inconsistent snapshot** → mitigated by reusing
  the #3217 backup artifact, which snapshots at a recorded `source_lsn` under
  the proper lock order.
- **Unauthenticated feed** → data exfiltration. Mitigation: shared-token
  handshake; TLS/at-rest concerns documented (network security is the
  operator's transport layer in v1).
- **Re-applying already-applied frames after replica restart** (sidecar/manifest
  lag) → handled: the replay engine's idempotency guards skip
  already-present versions.

### Six thinking hats

- **White (facts):** WAL entries are CRC-checked, LSN-ordered, framed into
  commit bands since #3413; `read_from(start_lsn)` exists and is cipher-aware;
  `.albk` records `source_lsn`; replay engine is idempotent and already used
  against live storage (startup, PITR); there is no listener precedent in-tree;
  MCP and HTTP each have exactly one dispatch seam with access classes already
  computed.
- **Red (gut):** the scariest part is replica apply racing readers, and hidden
  write paths bypassing the choke point. Both get dedicated tests. The success
  metrics (100K ops/s fixture) feel like lab numbers — treat them as
  benchmark-harness targets, not CI gates.
- **Black (risks):** apply-path bookkeeping (id generators, temporal index,
  timestamp seeding) is subtle — PITR's `finish_pitr_replay` is the checklist;
  missing one yields corrupt promotion. WAL truncation vs. shipping cursor is
  a true race; resolved conservatively (resync-required, never skip). Coverage
  gate: replication code is feature-gated out of the coverage run, so the gate
  is unaffected; always-compiled enforcement code gets direct tests.
- **Yellow (value):** pull-based design gives ~0% write-path overhead by
  construction; replicas carry the full bi-temporal surface — no incumbent's
  replica does; the `ReplicationSource` trait makes chaos tests deterministic
  (in-process source) while TCP covers real deployments.
- **Green (alternatives):** promotion as HTTP admin route (`POST
  /admin/promote`) for server deployments + Rust API for embedded; a
  `demote`/re-point path is documented in the runbook rather than automated;
  future: incremental segment tailing to cut feed read amplification, causal
  read-your-writes tokens, sync/quorum modes (out of scope per issue).
- **Blue (process):** TDD slices below; each slice red→green→refactor;
  conformance sweep + chaos test close AC2/AC8; AC matrix verified at the end
  against test names.

## Design

### Roles and enforcement (always compiled)

- `NodeRole { Primary, Replica }` held as an atomic on `AletheiaDB`
  (default `Primary` — standalone == primary with no replicas).
- `TransactionError::ReadOnlyReplica` rejected at:
  - `AletheiaDB::write_transaction()` / `write_transaction_with_options()`
  - `WriteTransaction::commit_with_timestamp_inner()` (promotion-race guard)
  - MCP `dispatch_tool` (after `authorize_tool`): access class `Write`/`Admin`
    → structured `FAILED_PRECONDITION`, `retriable: false`,
    `details: { node_role: "replica", reason: "read_only_replica" }`
  - HTTP `handle_query` (after `authorize`): write-class `QueryRequest` →
    same envelope via `AletheiaHttpError`.
- Promotion flips the atomic; `promote_to_primary()` stops the applier,
  seeds `wal.set_next_lsn(applied + 1)`, persists indexes, then flips.

### Replication engine (`src/storage/replication/`, feature `replication`)

- `ReplicationFeed` (primary): serves
  - `Handshake { token }` → role/ack
  - `FetchEntries { from_lsn, max_entries }` →
    `Entries { entries, primary_flushed_lsn, primary_wallclock_micros }` |
    `ResyncRequired { min_available_lsn }`
  - `FetchSnapshot` → streamed `.albk` bytes (built via `db.backup()` to a
    temp file) for initial sync.
- `ReplicationSource` trait: `handshake`, `fetch_entries`, `fetch_snapshot`.
  Implementations: `InProcessSource` (tests/chaos, wraps another `AletheiaDB`)
  and `TcpSource` (length-prefixed frames over `std::net::TcpStream`).
- `ReplicationServer` (primary): `std::net::TcpListener` accept loop thread,
  per-connection handler threads, shared-token auth, shutdown via
  `Arc<AtomicBool>` + `Drop` (repo's `FlushThread` idiom).
- `ReplicaApplier` (replica): background thread; loop:
  1. fetch entries from `applied_lsn + 1`
  2. resolve complete frames (`resolve_transaction_frames`)
  3. apply under `historical.write()` via the recovery replay engine
     (new `pub(crate)` live-apply wrapper doing PITR-style post-apply
     bookkeeping: id generators, temporal index, timestamp seeding)
  4. publish `applied_lsn`, lag metrics
  5. periodically persist indexes with manifest LSN = `applied_lsn`
- Wire format: existing WAL entry serialization (`serialize_entry` /
  segment payload codec) inside length-prefixed frames; entries CRC-verified
  on receipt.

### Observability

`DatabaseStats.replication: ReplicationStats` (serde free-ride into the MCP
`database_stats` tool): `role`, and for replicas `state`
(`connecting|streaming|resync_required|stopped`), `last_applied_lsn`,
`primary_flushed_lsn`, `entries_behind`, `lag_ms`, `last_error`.

### Consistency contract (AC3)

A replica applies whole commit frames in LSN order and publishes its applied
position only after a frame is fully applied. Therefore: any temporal query
anchored at transaction time ≤ the replica's applied position returns results
identical to the primary; current-state reads reflect the applied prefix of
the primary's history (bounded staleness = replication lag). Documented in
`docs/guides/replication.md` and asserted by parity tests.

### RPO / RTO (AC7)

- RPO (async mode) = replication lag at failure: bounded by
  `entries_behind` / `lag_ms`; measured by the lag harness.
- RTO = promotion latency: measured on the 10K nodes / 50K edges reference
  fixture by a scripted harness (`tests/replication_rto_harness.rs`,
  ignored-by-default long variant + CI-sized variant).

## TDD slices

- **A — roles + enforcement + stats skeleton** (always compiled):
  red tests for write rejection on all three surfaces, stats shape, error
  envelope; green; refactor.
- **B — engine**: feed, source trait, in-process source, applier, bootstrap
  from artifact, resync-required, applied-LSN persistence, promotion.
- **C — TCP transport**: framed protocol, token auth, snapshot streaming,
  reconnect/resume.
- **D — chaos + harness**: havoc test (kill primary mid-load via
  `mem::forget`, promote replica, verify integrity + bounded loss), RTO/lag
  measurement harness, CI wiring.
- **E — docs**: `docs/guides/replication.md` (setup, consistency contract,
  runbook incl. fencing, RPO/RTO), ADR-0059, CLAUDE.md, crash-scenarios entry.

## Out of scope (per issue)

Synchronous/quorum replication, automatic election, multi-primary,
cross-shard replication, SDK load balancing, geo tooling.
