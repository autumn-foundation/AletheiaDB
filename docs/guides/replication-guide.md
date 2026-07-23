# Asynchronous Replication Guide

AletheiaDB supports single-primary, asynchronous, pull-based replication:
one or more read-only replicas continuously stream a primary's
write-ahead log and apply it locally, giving you read scale-out and a warm
standby for manual failover. This guide covers setup, the consistency
contract, monitoring, security, and known limitations. For step-by-step
failover, see the [Promotion Runbook](promotion-runbook.md).

## What it is / when to use it

- **Read scale-out**: point read-heavy traffic (analytics, dashboards,
  LLM context queries) at one or more replicas instead of contending with
  writes on the primary.
- **Warm standby**: keep a continuously-updated copy of the database ready
  for manual promotion if the primary is lost.
- **Not**: synchronous/quorum replication, automatic failover, or
  multi-primary writes. Replication is **asynchronous** — a replica's data
  is always eventually consistent with the primary, never ahead of it, and
  possibly behind it by a bounded but nonzero amount (see
  [Consistency contract](#consistency-contract) below).

Out of scope entirely (per the design; see the "Alternatives considered"
section of [ADR-0059](../adr/0059-asynchronous-wal-shipping-replication.md)):
synchronous/quorum replication, automatic election/fencing, multi-primary,
cross-shard replication, and geo/SDK load-balancing tooling.

## Architecture summary

Replication ships **durable, already-flushed WAL entries** from the primary
to the replica; nothing is shipped until the primary itself has fsynced it
to a segment file, so a replica can never apply data the primary could still
lose on crash. The replica applies entries only in whole commit frames
(`[BeginTx .. CommitTx]` bands) via the same replay engine crash recovery and
point-in-time restore use — a transaction is either entirely applied or not
yet applied, never torn.

```text
┌─────────────────────────┐                      ┌─────────────────────────┐
│         Primary          │                      │         Replica          │
│                          │                      │                          │
│  Writes ──▶ WAL (fsync)  │                      │                          │
│               │          │   TCP, token auth    │                          │
│               ▼          │   (pull, polling)     │                          │
│      ReplicationFeed  ◀──┼───────────────────────┼──  TcpSource / applier   │
│      (durable entries    │   FetchEntries        │   fetch → resolve whole  │
│       only; never the    │   {from_lsn, max}     │   commit frames → apply  │
│       ring buffer)        │  ──────────────────▶  │   via recovery replay   │
│                          │   Entries |            │   engine → publish      │
│                          │   ResyncRequired       │   applied_lsn           │
│                          │                        │                          │
│      db.backup() (.albk) │◀───────────────────────│  FetchSnapshot           │
│      one-time bootstrap  │                        │  (initial sync only)     │
└─────────────────────────┘                        └─────────────────────────┘
```

Key properties:

- **Zero primary write-path overhead.** The primary's write/commit path has
  no replication-aware code in it at all; the feed only ever *reads* segment
  files a background connection asks for. Replication is entirely pull-based
  — the replica initiates every request.
- **Whole-frame apply.** The replica's applier
  (`src/storage/replication/applier.rs`) buffers fetched entries across
  polls and only advances its applied position after resolving a *complete*
  `[BeginTx .. CommitTx]` band (or a legacy/unframed entry, or a
  self-committing control op) — mirroring
  `crate::storage::recovery::resolve_transaction_frames`'s own acceptance
  checks (tx_id match, entry count, LSN contiguity, band brackets).
- **Reuses the recovery replay engine.** Applying replicated entries goes
  through the same `replay_entries_into_storage_with_constraints` used by
  crash recovery and PITR, with the same post-apply bookkeeping (id
  generator advancement, temporal-index rebuild, HLC-continuity seed).
- **Transport-agnostic core.** `ReplicationSource` (the trait a replica
  pulls from) has two implementations: `InProcessSource` (in-process, used
  by tests/chaos harnesses) and `TcpSource` (the real network transport).
  `ReplicationFeed` (primary side) is likewise transport-agnostic; the TCP
  server (`ReplicationServer`) just wraps it.
- **Native-only.** The replication engine (feed, source, applier, TCP
  transport) is compiled on every native target and is **not** gated behind
  a Cargo feature flag — it is simply absent on the ephemeral `wasm32`
  profile. The role atomic and write-rejection enforcement
  (`src/db/replication_role.rs`) is always compiled, on every target.

## Consistency contract

This is the part to internalize before relying on a replica for anything:

- **Replicas are strictly read-only.** Every write and admin surface —
  the Rust `AletheiaDB::write_transaction()`/`write_transaction_with_options()`,
  every MCP write/admin-class tool, and every HTTP write-class `/query`
  request — rejects on a replica with a structured, **non-retriable**
  `FAILED_PRECONDITION` error carrying
  `details: {"node_role": "replica", "reason": "read_only_replica"}` (both
  surfaces render byte-identical bodies; MCP: `src/mcp/auth.rs`'s
  `read_only_replica_error()`, HTTP: `src/http/error.rs`'s
  `AletheiaHttpError::read_only_replica()`). The Rust API surfaces the same
  condition as `TransactionError::ReadOnlyReplica`
  (`src/core/error.rs`). The **one** deliberate exception is
  `POST /admin/promote` itself (see the [runbook](promotion-runbook.md)) —
  every other write path keeps rejecting on a replica, including
  `POST /admin/keys` and `POST /admin/keys/revoke`.
- **Bounded staleness, never torn transactions.** A replica's applied
  position only ever advances past a *whole* commit frame. At any instant,
  every multi-operation transaction the primary committed is either fully
  visible on the replica or not visible at all — you will never observe half
  of a transaction's writes. This holds even under a transport that delivers
  the frame's entries split across multiple polls.
- **Bi-temporal reads are consistent-as-of the applied position.** A
  temporal query (`AS OF`, `get_node_at_time`, etc.) run against a replica at
  transaction time ≤ the replica's `last_applied_lsn` coordinate returns
  results identical to the same query against the primary. Current-state
  reads reflect the applied prefix of the primary's history — i.e., bounded
  staleness equal to replication lag, never divergence.
- **RPO under primary loss = replication lag at the moment of failure.**
  If the primary is lost, any writes committed on the primary after the
  most-recently-applied replica position are gone. That gap is exactly what
  `entries_behind`/`lag_ms` report (see [Monitoring](#monitoring)) — watch
  them to know your current data-loss exposure. For a reproducible
  measurement harness, see `tests/replication_slo_harness.rs`.

  Representative measurements from that harness (Linux CI-class hardware;
  reproduce with `cargo test --test replication_slo_harness -- --ignored`
  for the reference fixture):

  | Metric | CI-sized fixture | Reference (10K nodes / 50K edges) | Target |
  |--------|------------------|-----------------------------------|--------|
  | Promotion latency (RTO, engine-side) | 1–9 ms | 17–23 ms | < 10 s |
  | Replication lag p50 / p99 (sustained load) | 21 ms / 254 ms | 5.5 s / 18.1 s¹ | p99 < 10 s (CI-sized) |
  | Primary write-path overhead with attached replica | ≈ 0 (within noise) | ≈ 0 (within noise) | < 25 % |

  ¹ Reference-load lag reflects the current per-batch temporal-index rebuild
  on the replica (a documented performance follow-up in the applier); it is
  printed by the harness but not asserted.

- **A paused/disconnected replica simply stops advancing** — it does not
  serve stale-but-moving data incorrectly, it serves a **frozen**,
  internally consistent snapshot at its last applied position until the
  connection resumes.

## Quick start

### Config file (TOML)

Primary — accept replica connections:

```toml
[replication]
listen_addr = "0.0.0.0:4460"
auth_token_env = "ALETHEIADB_REPLICATION_TOKEN"
```

Replica — stream from that primary:

```toml
[replication]
primary_addr = "primary.internal:4460"
auth_token_env = "ALETHEIADB_REPLICATION_TOKEN"
poll_interval_ms = 50
batch_max_entries = 500
```

These are the exact fields of `ReplicationConfig` (`src/config.rs`):
`listen_addr`, `primary_addr`, `auth_token`, `auth_token_env`,
`poll_interval_ms` (default `50`), `batch_max_entries` (default `500`). Both
`listen_addr` and `primary_addr` may be set at once (a replica that also
serves further downstream readers). Setting either without a resolvable
token (`auth_token_env` takes precedence over the inline `auth_token`) fails
`with_unified_config` fast at startup with a `ConfigError::InvalidValue` —
there is no silent anonymous-replication fallback.

> **Bootstrap caveat for the config-driven `listen_addr` path.** When
> `with_unified_config` auto-starts a listen server from `listen_addr`, the
> database object isn't wrapped in an `Arc` yet, so that auto-started server
> can only serve `FetchEntries` (streaming) — it replies "snapshot
> unavailable" to a fresh replica's bootstrap request. To serve initial
> snapshot bootstrap too, call
> `ReplicationServer::start(Arc::new(db), listen_addr, token)` explicitly
> with a real `Arc<AletheiaDB>` (see below) instead of relying solely on
> config auto-wiring.

### Programmatic Rust quick start

The snippets below are illustrative (matching this codebase's convention for
API examples) — see `src/db/replication.rs` and
`src/storage/replication/` for exact signatures.

```rust
use aletheiadb::{AletheiaDB, ReplicationServer, TcpSource, ReplicationOptions};
use std::sync::Arc;

// --- Primary: serve both streaming and snapshot bootstrap ---
let primary = Arc::new(AletheiaDB::open("/var/lib/myapp/primary")?);
let _server_handle = ReplicationServer::start(
    Arc::clone(&primary),
    "0.0.0.0:4460",
    std::env::var("ALETHEIADB_REPLICATION_TOKEN")?,
)?;

// --- Replica: bootstrap from scratch, then stream ---
let source = Box::new(TcpSource::new(
    "primary.internal:4460",
    std::env::var("ALETHEIADB_REPLICATION_TOKEN")?,
));
let replica = AletheiaDB::bootstrap_replica(
    source,
    std::path::Path::new("/var/lib/myapp/replica"),
    ReplicationOptions::default(),
)?;

// --- Or: an already-open (possibly previously-bootstrapped) replica just resumes ---
let replica = AletheiaDB::open("/var/lib/myapp/replica")?;
let source = Box::new(TcpSource::new("primary.internal:4460", token));
replica.start_replication(source, ReplicationOptions::default())?;

// --- Observe progress ---
if let Some(progress) = replica.replication_progress() {
    println!("state={} applied_lsn={} entries_behind={:?} lag_ms={:?}",
        progress.state, progress.last_applied_lsn,
        progress.entries_behind, progress.lag_ms);
}
```

`ReplicationOptions` (`src/storage/replication/applier.rs`) controls the
applier's cadence: `poll_interval` (default 50ms), `batch_max_entries`
(default 500), `persist_every` (default `Some(5s)`), and
`persist_every_entries` (default 500) — the latter two govern how often the
replica persists its indexes (stamped with the replica's own applied-LSN
coordinate) so a restart resumes without a full re-stream.

## Bootstrap & resume

- **Initial bootstrap** (`AletheiaDB::bootstrap_replica`) fetches a
  consistent point-in-time snapshot from the primary — the exact same
  `.albk` backup artifact format described in the
  [Backup & Restore guide](backup-restore.md) — restores it via
  `AletheiaDB::restore_to_data_dir`, which durably records the snapshot's
  `source_lsn`, and then calls `start_replication`, which resumes streaming
  from precisely that LSN.
- **Restart / resume**: `start_replication` resumes from this database's
  startup index-manifest LSN — the coordinate captured when indexes were
  loaded at `open()`/`with_unified_config` time — or LSN 1 for a fresh
  database with no manifest. Combined with the applier's periodic
  index-persistence (stamped at the replica's own `applied_lsn`, a
  primary-space coordinate distinct from the replica's local WAL LSN space,
  which the applier never appends to), a restarted replica process resumes
  close to where it left off instead of re-streaming from scratch.
- **Reconnect**: `TcpSource` connects lazily and re-handshakes on every
  reconnect. Any I/O or protocol error drops the current connection; the
  applier's existing poll/backoff loop simply retries the next call, which
  transparently reconnects.
- **`resync_required`**: the primary's feed compares a requested `from_lsn`
  against its minimum retained (not-yet-truncated) WAL segment LSN. If the
  requested LSN has already fallen off retained history — because WAL
  segments were truncated past what this replica still needs (tiered-storage
  `truncate_to_lsn`, a retention sweep) — the primary returns a structured
  `ResyncRequired { min_available_lsn }` instead of silently skipping ahead
  (which would corrupt the replica's history). The applier surfaces this as
  `state: "resync_required"`, freezes its applied position, and **stops
  applying**; it does not recover on its own. **Operator action**:
  re-bootstrap the replica from a fresh snapshot
  (`AletheiaDB::bootstrap_replica` against a fresh data directory, or wipe
  the existing one first).

## Monitoring

`AletheiaDB::replication_progress()` returns `Option<ReplicaProgressStats>`
(`src/db/stats.rs`) — `None` when no applier is currently running (a plain
primary, or a replica on which replication was never started). Fields:

| Field | Meaning |
|---|---|
| `state` | `"connecting"`, `"streaming"`, `"resync_required"`, or `"stopped"` |
| `last_applied_lsn` | The last LSN this replica has fully applied |
| `primary_flushed_lsn` | The primary's last-known flushed LSN, when reported |
| `entries_behind` | `primary_flushed_lsn - last_applied_lsn`, when both known |
| `lag_ms` | Estimated replication lag in milliseconds, when derivable |
| `last_error` | The most recent applier error message, if any |

The same data is folded into `AletheiaDB::stats()` under
`DatabaseStats.replication: ReplicationStats { role, replica }` — `role` is
`"primary"`/`"replica"` (an O(1) atomic read), `replica` is populated
whenever `role == "replica"` (`None` on a primary). This is exactly what the
MCP `database_stats` tool returns (no arguments; see the
[MCP query tool guide](mcp-query-tool.md#database-stats-and-storage-tier-health-database_stats))
— an LLM/operator can call it against a replica to check `entries_behind`
and `lag_ms` before trusting a read, or against a candidate promotion target
before failing over (see the [Promotion Runbook](promotion-runbook.md)).

## Security

- **Token authentication only, no TLS in v1.** Every connection performs a
  handshake (`MSG_HELLO {token, protocol_version}`) that the primary
  verifies with a **constant-time** SHA-256 digest comparison
  (`subtle::ConstantTimeEq`, mirroring the API-key verification in
  `src/auth/store.rs`) before serving any request; a mismatched token gets
  `MSG_AUTH_FAILED` and the connection is closed. **The wire protocol itself
  is plaintext** — there is no encryption of the TCP link in v1. Run
  replication only over a trusted network segment (a private VPC/subnet,
  a VPN, or an SSH/stunnel tunnel you manage yourself); do not expose
  `listen_addr` to an untrusted network.
- **Prefer `auth_token_env` over the inline `auth_token`.** `auth_token_env`
  names an environment variable resolved at startup and takes precedence
  over `auth_token` when both are set — the token itself never has to touch
  a config file. `auth_token` remains available for local development and
  tests.
- **The token is never logged.** Both the client (`TcpSource`) and server
  drop the plaintext token from memory immediately after hashing it for
  comparison/storage.
- **`POST /admin/promote` is Admin-class.** Per the
  [access control matrix](access-control-matrix.md), promotion requires an
  `admin`-role API key on the HTTP surface, same as key lifecycle
  management.

## Limitations

- **Single primary, manual failover only.** There is no automatic election,
  quorum, or consensus — an operator (or their own tooling) decides when to
  promote a replica, following the [Promotion Runbook](promotion-runbook.md).
- **No automatic fencing — split-brain risk.** Promoting a replica does
  **not** stop the old primary from accepting writes if it is still running
  (e.g., a network partition rather than a true crash). If both nodes accept
  writes, you now have two divergent histories. **The operator must isolate
  (fence) the old primary — stop the process or block its network access —
  before or as part of promotion.** See the runbook's fencing step.
- **Lineage index (#3371) is in-memory only.** The derivation-lineage index
  does not survive a process restart on either a primary or a replica (this
  is a pre-existing, general lineage limitation, not replication-specific)
  — it is rebuilt from nothing on restart, so lineage queries against a
  freshly-restarted replica will not reflect lineage recorded before that
  restart.
- **Temporal-index rebuild is O(total versions) per applied batch, not O(delta).**
  The replica applier rebuilds its temporal index from all versions after
  every applied batch (`rebuild_temporal_index_from_versions`, mirroring
  PITR's `finish_pitr_replay` exactly) rather than incrementally wiring in
  just the newly-applied versions. This is fine for typical workloads today
  but means a replica with very deep history and a very busy primary will
  spend proportionally more CPU per batch; an incremental version is a
  documented perf follow-up.
- **WAL retention on the primary must cover expected replica downtime.**
  If a replica is disconnected longer than the primary retains WAL segments
  before truncation (tiered-storage migration, retention sweep), it will
  come back to a `resync_required` state and need a fresh bootstrap rather
  than resuming — see [Bootstrap & resume](#bootstrap--resume) above. Size
  your WAL retention window to your expected maximum replica outage.
- **Server connection cap.** A `ReplicationServer` serves at most
  `MAX_CONCURRENT_CONNECTIONS` (16) connections concurrently; connections
  beyond that are refused immediately with no response. Frame payloads are
  capped at `MAX_FRAME_SIZE` (64 MiB) on both sides as a sanity bound against
  a malformed/hostile peer.

## Further reading

- [Promotion Runbook](promotion-runbook.md) — step-by-step manual failover.
- [ADR-0059: Asynchronous WAL-Shipping Replication](../adr/0059-asynchronous-wal-shipping-replication.md) — design rationale and alternatives considered.
- [Crash scenarios index](../testing/crash-scenarios.md) — chaos/crash test coverage for replication (torn frames, primary loss mid-load, resync).
- `tests/replication_slo_harness.rs` — reproducible RPO/RTO measurement harness.
- `tests/replication_engine.rs` / `tests/replication_tcp.rs` — the behavioral
  contract tests referenced throughout this guide (bounded staleness,
  torn-frame safety, resync-required, reconnect/resume, promotion).
- [Design plan: Issue #3355](../plans/2026-07-23-3355-async-replication.md) and
  [SPEC-011](../specs/011_distributed_replication.md) — full design background.
