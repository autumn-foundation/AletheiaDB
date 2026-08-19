# Changelog

All notable changes to AletheiaDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### ⚠️ Breaking changes

- WAL config structs gained fields (Issue #3798), so exhaustive struct-literal
  construction that names every field no longer compiles — add
  `..Default::default()`, or use the builders. Affected:
  `crate::config::WalConfig` (`max_append_block_ms`, `acquire_timeout_ms`),
  `ConcurrentWalSystemConfig` (both), `ConcurrentWalConfig`
  (`max_append_block_ms`), and `GroupCommitConfig` (`acquire_timeout_ms`).
  Every new field defaults to the pre-#3798 behavior for a healthy system, so
  this is a compile-time break only — see Added/Changed below for what the
  fields do.

### Added

- WAL stall diagnosability (Issue #3798). Two additive config fields:
  `ConcurrentWalSystemConfig::max_append_block_ms` /
  `ConcurrentWalConfig::max_append_block_ms` (default 30_000; `0` = unbounded)
  bounding how long a writer blocks on a full ring buffer, and
  `GroupCommitConfig::acquire_timeout_ms` (default 120_000; `0` = unbounded)
  bounding acquisition of the group-commit state mutex (see Breaking changes
  above). Setters `with_max_append_block_ms` (both WAL config types) and
  `with_acquire_timeout_ms` (`ConcurrentWalSystemConfig`) are provided.
- Both bounds are reachable end-to-end from the user-facing configuration
  (Issue #3798): `crate::config::WalConfig::max_append_block_ms` /
  `acquire_timeout_ms`, the matching `WalConfigBuilder` setters, and the
  `[wal]` TOML keys of the same names (`#[serde(default)]`, so existing config
  files load unchanged). `acquire_timeout_ms` reaches the coordinator via
  `GroupCommitCoordinator::with_config`, so the documented
  `0` = legacy-unbounded escape hatch now works from `AletheiaDB::open` /
  `with_unified_config`, not only from the internal config types.
- `ConcurrentWalSystem::flush_heartbeat()` (completed background-flusher loop
  iterations; strictly increasing while the flush thread is alive) and
  `flush_cycle_errors()` (process-lifetime count of non-poison flush-cycle
  errors survived instead of dying), Issue #3798.
- `GroupCommitCoordinator::reentrancy_detections()` and `acquire_timeouts()`
  counters, so a health check or soak test can assert both stay at zero
  (Issue #3798). Re-entrancy detection is non-blocking and precedes every
  acquisition; `ALETHEIADB_WAL_REENTRANCY_PANIC=1` escalates a detection from
  an error to a panic for CI. The variable is read once per process, but the
  resulting policy is stored **per coordinator** (captured at construction), so
  the flag and the re-entrancy tests coexist — each test pins the behavior it
  asserts on its own instance instead of inheriting the job's environment.

### Changed

- **BEHAVIORAL:** WAL appends can now fail instead of blocking indefinitely
  (Issue #3798). When a stripe ring buffer stays full for
  `max_append_block_ms` (default 30s), the append returns a structured
  `StorageError::WalError` naming the stripe, the wait, the bound, and the
  likely cause ("the background flusher may be dead or wedged") rather than
  parking the writer forever. Buffer **closed** still takes precedence over the
  deadline and keeps its historical `"WAL buffer closed"` wording. Set
  `max_append_block_ms: 0` to restore the unbounded legacy behavior. The
  classification is conservative: the timeout is a `StorageError::WalError`,
  which the MCP surface maps to `INTERNAL` / `retriable: false`, even though a
  ring-buffer-full timeout is in fact retry-safe (a mid-batch failure leaves a
  prefix with no `CommitTx` marker, which recovery discards). A dedicated
  retriable variant is tracked as Issue #3800.
- **BEHAVIORAL:** the **synchronous and handle-based** append paths
  (`append_with_handle`, `append_batch_with_handles`) are bounded by the same
  `max_append_block_ms` (Issue #3798) — they were the remaining unbounded
  entries into the ring buffer, and in `Synchronous` mode nothing drains in the
  background, so a full ring could self-deadlock the writer. The error text is
  mode-appropriate there ("no consumer is draining the buffer …") rather than
  blaming a flusher that does not exist. The attribution follows the
  **durability mode**, not the append family: the write-transaction path uses
  the *async* batch append in every mode, so a stalled async append in
  `Synchronous` mode also names the calling thread instead of sending the
  operator to `is_healthy()`, which that mode reports as `true` by
  construction.
- **BEHAVIORAL:** the append bound measures **time without progress**, not
  total call time (Issue #3798). One deadline is shared by every entry of a
  batch, so consecutive blocked entries are one stall window rather than one
  each, and each entry actually placed restarts the window; the deadline is
  lazy, so no clock is read on the never-blocking fast path. There is
  deliberately **no cap on total batch time**: a large batch that keeps getting
  room completes however long it takes (an earlier per-call cap failed slow
  bulk imports mid-flight and blamed a flusher that was draining the whole
  time). An absurd configured value whose deadline instant is unrepresentable
  degrades to "unbounded" instead of panicking the writer; the same applies to
  `acquire_timeout_ms`.
- **BEHAVIORAL:** `ConcurrentWalSystem::is_healthy()` now reports `false` on a
  stale flush heartbeat, not only on outstanding consecutive flush errors
  (Issue #3798) — a flusher that died or wedged raises no error at all, so
  error counters alone could never see it. Staleness threshold is
  `max(10 × flush_interval, 5s)` — the 5s floor keeps a very small
  `flush_interval_ms` from turning an ordinary scheduling hiccup on a loaded CI
  box into a "dead flusher" verdict. The heartbeat is seeded at construction so
  startup is a grace period, and a durability mode with no background flusher
  (e.g. `Synchronous`) is never stale. This value also feeds
  `database_stats.wal.healthy`.
- **BEHAVIORAL:** flusher heartbeat timestamps are now **monotonic** —
  microseconds since a process-start `Instant` baseline rather than the
  wallclock (Issue #3798) — so a system clock step or a suspend/resume can
  neither invent staleness nor hide it.
- **BEHAVIORAL:** group-commit state-mutex acquisition is now bounded by
  `acquire_timeout_ms` with non-blocking re-entrancy detection (Issue #3798).
  Both refusals are `StorageError::WalError` (`INTERNAL`, non-retriable on the
  MCP surface) carrying a forensic payload — acquiring site, time waited
  against the bound, holder thread token and holder site, waiter count, and
  `current_epoch`/`flushed_epoch`/`batch_count` mirrors — sampled without the
  mutex, so it is a triage lead rather than an authoritative read. The default
  (120_000) is strictly greater than the `timeout_max_ms` default (60_000) so
  the `wait_for_flush` deadlock detector always fires first; this is deadlock
  detection, not a performance SLA. `0` restores unbounded `Mutex::lock`.
  Lock poisoning is unchanged and still maps to `StorageError::LockPoisoned`.
- A transient (non-poison) group-commit coordinator error no longer kills the
  background flush thread (Issue #3798): the cycle is counted, logged, and
  retried on the next interval; only a poisoned coordinator lock still fails
  fast. Flush-path logging no longer uses `eprintln!`, which panics when
  stderr is closed (EPIPE) — killing the flush thread is precisely the failure
  that path exists to report.
- The background flusher now splits `start_flush` / `finish_flush` and retains
  a refused `finish_flush` on a FIFO queue of un-finished epochs, drained
  oldest-first at the top of each subsequent cycle (Issue #3798). Previously a
  single bounded-acquisition failure inside `finish_flush` could permanently
  wedge the group-commit epoch chain and the durability frontier, leaving every
  waiter parked behind it. `GroupCommitCoordinator::mark_flushed` consequently
  has no production caller any more and is retained as test /
  back-compatibility API.

### Fixed

- **DURABILITY:** a flush outcome is never discarded (Issue #3798). While an
  un-finished epoch was outstanding, the flusher declined to open a new epoch
  and **dropped that cycle's outcome** — including a FAILED disk flush, which
  then reached nobody; a later cycle published a clean success for the very
  epoch whose entries never reached disk, so its committers were told a lost
  transaction was durable. The flusher now always opens an epoch and always
  delivers its outcome (a queued epoch defers *delivery*, never suppresses the
  *outcome*), which is safe because `finish_flush` already parks out-of-order
  completions and records an error against its own epoch. A failure whose
  `start_flush` was refused is likewise carried into the next epoch that opens,
  since `current_epoch` did not move and that epoch covers the same
  transactions. A coordinator lock unusable for ~1024 consecutive cycles now
  fails fast rather than silently dropping outcomes or growing without bound.
- `GroupCommitCoordinator::contended_acquires()` is now readable (Issue #3798);
  the counter was incremented but exposed nowhere, so a rising ordinary
  contention could not be told apart from a suspected deadlock. It also appears
  in the acquisition-failure forensic payload.

- Documented the durability-unknown caveat on commit-path acquisition timeouts
  (Issue #3798): a timeout at `register_transaction` or `wait_for_flush` may
  leave a WAL frame already appended, so the transaction **may still become
  durable and replay at recovery** and must not be reported to a client as a
  clean abort. The WAL transaction frame format that would make this precise
  already shipped in 0.2.0 (Issue #3413 — `BeginTx`/`CommitTx`/`AbortTx`); what
  is still missing is the commit path **emitting** an abort record when a step
  after the append fails, tracked as Issue #3799. See
  [docs/WAL.md](docs/WAL.md) ("Stall Diagnosability") and the module
  documentation on `src/storage/wal/group_commit.rs` for the full call-site
  audit.

## [0.2.0] - 2026-07-30

First crates.io release since 0.1.1. This release ships the trunk work
accumulated since 0.1.1 as 0.2.0. Under 0.x SemVer, any breaking public-API
change forces a minor bump, and there are several (see Breaking changes below),
so a minor bump off 0.1.x is mandatory. MSRV: Rust 1.92, edition 2024. License:
MIT OR Apache-2.0.

**Upgrading from 0.1.x?** See the [0.1 → 0.2 migration guide](docs/guides/migration-0.1-to-0.2.md).
**Drain your WAL before upgrading** — a 0.1.x data directory with an unreplayed
pre-v13 WAL tail is now refused on open (see On-disk format below).

### ⚠️ Breaking changes

- `ReadOps::get_outgoing_edges`, `ReadOps::get_incoming_edges`, and
  `ReadOps::get_outgoing_edges_with_label` now return `Result<Vec<EdgeId>>`
  instead of `Vec<EdgeId>` (Issue #359). A node that does not exist (or is not
  visible in the transaction's snapshot) returns `Err(NodeNotFound)`,
  consistent with `get_node`/`get_edge`; an existing node with no matching
  edges returns `Ok(vec![])`, so callers can distinguish "node has no edges"
  from "node doesn't exist". Migration: append `?` (or `.unwrap_or_default()`
  to keep the old silent-empty behavior) at call sites. The non-transactional
  `AletheiaDB` convenience methods of the same name are unchanged.
- The public error enums are **not** `#[non_exhaustive]` and gained variants,
  so any exhaustive `match` over them now needs a wildcard arm. Notably
  `TransactionError::ValidationFailed` (the #3416 concurrent-orphan-edge
  commit abort), new `ConstraintError` variants (#3378 schema constraints),
  and `Error::Namespace`/`Provenance`/`Lineage`/`Constraint`/`Backup`/
  `FailedPrecondition` (#3349/#3371/#3378/#3351) were added to the top-level
  `Error` and its siblings (`StorageError`, `TransactionError`,
  `ConstraintError`). Late in the cycle the top-level `Error` further gained
  `Tenant` (#3365), `ResourceExhausted` (#3368/#3365), `ReadOnlyReplica`
  (#3355), `PreV13WalTailRequiresMigration` (#3746), `FenceTooLow` (#3755),
  and `IndexKeyringAlreadyInstalled`/`ColdKeyringAlreadyInstalled` (#3708).
- `PersistenceConfig` gained a public field `max_interned_strings: usize`
  (Issue #3716). Struct-literal constructors that name every field break;
  use `..Default::default()`.
- `PersistenceConfig::default()` no longer enables index persistence
  (`enabled: false`, Issue #3388) — a behavioral break. A config that never
  touches `PersistenceConfig` no longer writes index snapshots into a
  cwd-relative `./data`. Opt in explicitly with `enabled: true` + an explicit
  `data_dir`, or use the canonical durable entry points `AletheiaDB::open(path)`
  / `durable_config_for_data_dir(path)` (unaffected; WAL durability is
  independent).
- MCP error responses are now a structured object
  `{"error": {"code", "message", "retriable", "details"?}}` instead of
  `{"error": "<string>"}` (Issue #3234) — a wire break for MCP consumers that
  read `error` as a string. The prior free text is preserved verbatim at
  `error.message`.
- The HTTP error envelope was unified to the same nested `{"error": {...}}`
  shape and the legacy flat body (`{"success": false, "error": "<msg>", ...}`)
  was **removed** (Issue #3234) — a wire break for HTTP clients. HTTP and MCP
  error bodies are now byte-shape-identical; success responses
  (`{"success": true, "data": ...}`) are unchanged.
- Opening a 0.1.x data directory that still holds an **unreplayed pre-v13 WAL
  tail** is now **refused** (`Error::PreV13WalTailRequiresMigration`,
  `FAILED_PRECONDITION`, Issue #3746) instead of succeeding. This replaces
  silent data corruption: pre-v13 segments store labels as raw process-local
  interner ids, which 0.2.0 rebuilds in a different order, so replaying such a
  tail resolved labels to unrelated strings. Migration: drain the WAL under
  0.1.x (clean shutdown / checkpoint) before upgrading — a cleanly-checkpointed
  0.1.x directory opens under 0.2.0 with full integrity. See "Before you
  upgrade (data safety)" in the
  [migration guide](docs/guides/migration-0.1-to-0.2.md).

### On-disk format

- Backup artifact `.albk` bumped to **v7** (folds the crypto-shred keyring +
  subject-designation registry — #3712/#3715 — alongside the #3218
  unique-constraint registry and #3378 schema constraints). The reader still
  decodes v1–v6, so a new binary reads old backups; a v7 backup is **not**
  readable by a ≤0.1.1 binary (forward-incompatible).
- Encryption-at-rest on-disk **state v2** plus keyring / crypto-shred
  designation registry (Issue #3616/#3359) — new persisted structures with no
  0.1.1 equivalent, present only when the `encryption` feature is in use.
- WAL segment format **v13** (plaintext) / **v14** (encrypted) carry node and
  edge labels as strings rather than raw interner ids (Issue #3506), which is
  what makes replay correct under any interner layout. A separate
  variable-length `KEYVERSIONED` container (**v16**) carries a per-segment
  `key_version` so full-MEK rotation can write new appends under a new WAL DEK
  while legacy segments still replay under the old one (Issue #3617). Older
  binaries reject these versions cleanly rather than misparsing them; 0.2.0
  still *reads* ≤v12 segments, but refuses to *replay* a pre-v13 tail (see
  Breaking changes).
- WAL commit **and abort** framing (`[BeginTx, ..ops.., CommitTx]` /
  `AbortTx`, Issue #3413): a transaction whose frame is durably fsync'd and is
  *then* rejected at the commit guard is no longer re-applied by crash
  recovery. Previously such a runtime-refused write could be resurrected on
  replay (a lost CAS/lease claim, a stale fence, an orphaning delete).
- Index-persistence manifest v3 and cold-storage record tag v3 were bumped
  **backward-compatibly** to carry `provenance.principal` (Issue #3350). Older
  artifacts still load, with `principal: None`.

### Added

#### Daemon-owned database (Issue #2905)

- A new `aletheia-daemon` binary makes one process the local owner of the WAL,
  indexes, and recovery, serving REST + **MCP over Streamable HTTP (`/mcp`)** +
  OpenAPI + `/metrics` from a single autumn-web app, and claiming
  `{data_dir}/daemon.lock` so a second
  daemon on the same directory refuses to start. `aletheia daemon start` launches
  it by default (`--surface legacy` keeps the previous HTTP-only
  `aletheia-server`), and `aletheia daemon status [--json]` reports the base URL,
  the MCP endpoint, and a paste-ready MCP client configuration. `aletheia-mcp`
  gains a **daemon-client mode** (`ALETHEIADB_DAEMON_URL` / `--daemon-url`): a
  stdio↔HTTP JSON-RPC relay that never opens local storage, so many MCP client
  sessions share one database instead of each opening (or inventing) their own.
  Embedded stdio mode is unchanged when no daemon URL is configured. The
  ownership claim is honored by every storage-opening process (the CLI, embedded
  `aletheia-mcp`, `aletheia-server`), so a misconfigured client refuses to start
  rather than silently becoming a second writer. Daemon liveness and stop now
  work on Windows and on Unix systems without `/proc` (macOS), not only Linux.
  Install the daemon with `cargo install --path crates/aletheia-server`.
  See `docs/guides/daemon-mode.md`.

#### Multi-tenant isolation (Issue #3365)

- `TenantManager` serves many isolated logical databases from one process,
  owning **one fully-separate `AletheiaDB` per tenant** — so hard data
  isolation, per-tenant history/indexes/constraints/schema, independent
  `.albk` backup/restore, and blast-radius containment fall out by
  construction rather than from leak-prone per-query filtering. Lifecycle
  (`create_tenant`/`get_tenant`/`list_tenants`/`delete_tenant`/
  `restore_tenant`), a session-binding `TenantHandle` (including a
  tenant-scoped `mcp_server()`), and O(1) `TenantUsage` accounting.
- Adjustable `TenantQuota`s: `max_nodes`/`max_edges` enforced **precisely**
  via an atomic reserve-before-write / release-on-failure (never a partial
  write, never a race past the cap); `max_vector_index_bytes`/
  `max_storage_bytes` enforced best-effort against O(1) estimators. A breach
  is `RESOURCE_EXHAUSTED` with `details: {tenant, dimension, current, limit}`
  on both the MCP and HTTP envelopes.
- Tenant ids are lowercase-only (`[a-z0-9._-]`) so a case-insensitive
  filesystem cannot collide two tenants onto one WAL. The single-tenant
  default (`AletheiaDB::new()`/`open()`) is untouched, zero overhead.
  See [docs/guides/multi-tenancy.md](docs/guides/multi-tenancy.md).

#### Asynchronous replication (Issue #3355)

- Single-primary, asynchronous, pull-based replication: a replica polls the
  primary's feed for durable (already-fsynced) WAL entries and applies them
  through the same recovery replay engine crash recovery uses, giving read
  scale-out and a warm standby with effectively zero primary write-path
  overhead. Entry points `AletheiaDB::start_replication` and
  `bootstrap_replica` (snapshot-bootstrap via `.albk`, then stream),
  configured by `[replication]` / `ReplicationConfigBuilder`.
- Replicas are strictly read-only: every write/admin surface rejects with a
  non-retriable `FAILED_PRECONDITION`
  (`details: {node_role: "replica", reason: "read_only_replica"}`). Only
  **whole commit frames** are submitted for apply, so a replica never durably
  stops mid-transaction and its state after each batch is a consistent,
  possibly-stale snapshot. Manual failover via
  `promote_to_primary()` / `POST /admin/promote`; lag and RPO are surfaced by
  `replication_progress()` / `database_stats`. No automatic election or
  fencing. See [docs/guides/replication-guide.md](docs/guides/replication-guide.md)
  and [docs/guides/promotion-runbook.md](docs/guides/promotion-runbook.md).

  **Known issue:** whole-frame submission is not the same as reader isolation
  *during* an apply. `apply_replica_batch` replays a complete frame into the
  current-state storage entry by entry while holding only the `historical`
  write lock, which current-state reads do not take — so a read concurrent
  with an in-progress apply can observe part of a transaction (one node of a
  two-node commit). The window is microseconds and rarely observed, but
  `torn_frame_safety_never_exposes_a_partial_transaction` does catch it
  intermittently under CI load. Reads on an idle replica, and all durability
  and post-promotion guarantees, are unaffected.

#### Semantic analysis over bi-temporal history

- Temporal semantic drift alarms (Issue #3367): declare monitors and the
  database watches its own embedding evolution, firing durable, queryable,
  changefeed-delivered alarms when meaning drifts past a threshold — instead
  of drift being visible only when a human thinks to ask afterwards.
  (`semantic-temporal`.)
- Contradiction genealogy (Issue #3352): reconstruct how conflicting claims
  about a fact evolved across bi-temporal history and provenance, plus a
  `find_contradictions` scan for entity/property contradictions.
  (`semantic-temporal`.)
- Counterfactual exclusion replay (Issue #3357): materialize a **read-only**
  counterfactual view that excludes one source's writes and report the blast
  radius. The real database is never mutated; responses carry a
  `counterfactual: true` marker. (`semantic-temporal`.)
- Trust propagation over derivation lineage (Issue #3382): computed
  confidence as a tree over a fact's derivation lineage, with per-label
  policy and a `trust_breakdown` explainability surface.
  (`semantic-reasoning`.)
- Valid-time-aware trust evaluation (#3382 follow-up): new public
  `AletheiaDB::computed_confidence_as_of_bitemporal` plus
  `TrustOptions::as_of_valid_time` / `TrustOptions::with_as_of_valid_time`, so
  computed confidence over lineage can be evaluated at a caller-supplied
  valid-time coordinate (defaulting to wallclock now when unscoped).
- Knowledge half-life analytics (Issue #3377): read-only survival analysis
  over bi-temporal version history, measuring how long facts of a given kind
  stay true — so an agent can refresh volatile facts and stop over-refreshing
  stable ones.

#### Encryption suite

- Durable encryption-state authority establishing the on-disk source of truth
  for the database's encryption posture (Issue #3616, PR 1 of 4).
- WAL runtime-installable keyring: the write-ahead log can transition from
  plaintext to encrypted while running (Issue #3616 PR2), with keyring
  provisioning at `open()` (Issue #488/#3653).
- `enable_encryption(&mut self, KeyProviderConfig) -> Result<EnableReport>`
  performs an in-place plaintext→encrypted migration, and
  `disable_encryption(&mut self) -> Result<DisableReport>` performs the
  encrypted→plaintext reverse (Issue #3616 PR3/PR4).
- Cold-tier (redb) key rotation, completing full-MEK all-layer key rotation
  across every storage layer (Issue #3617 PR3 of 3).
- New feature flags: `encryption`, `encryption-aws-kms`, `encryption-vault`.

#### GDPR crypto-shred (Issue #3359)

- Subject-key axis foundation for per-subject cryptographic erasure.
- Seal-at-write / unseal-at-read property-path integration, with a
  fail-closed erase-vs-seal race hardening and a public erased accessor.
- Provenance-chain erasure stability (a shredded subject leaves the
  tamper-evident chain verifiable).
- CLI support, plus MCP admin tools `designate_subject` and
  `erase_subject` (tool registry 61→63), with a 1000-target DoS cap on
  designation.
- The keyring + designation registry are folded into `.albk` backups (format
  v7).

#### Namespaces (Issue #3349)

- Core registry model with reserved-key ride-along and elision.
- Storage/query threading: a membership index, namespace-scoped reads, and a
  traversal boundary that respects namespace membership.
- MCP/HTTP namespace parameters and per-namespace counts, plus the
  `create_namespace` / `list_namespaces` / `describe_namespace` MCP tools
  (registry 58→61).
- `ChangeFilter.namespace` for namespace-scoped changefeed subscriptions.

#### Changefeed (Issues #3375, #3216, #3652, #3673, #3678)

- `AletheiaDB::subscribe_changes` in-process subscription primitive with a
  bounded buffer, best-effort at-least-once delivery, and lossless resume via
  `list_changes` (Issue #3375).
- `await_changes` MCP long-poll tool plus the HTTP SSE `GET /changes/stream`
  route (Issue #3652).
- Event-driven await: no worker pinned during the block, prompt slot release
  (Issue #3673).
- Per-principal subscription quota (Issue #3678).
- Filter + limit pushdown into the `list_changes` hot/cold scans (Issue #3216).

#### Query languages (Issues #3622, #558, #557, #548)

- Edge-property `WHERE` + `ORDER BY` predicates for both AQL and Cypher
  (Issue #3622), with consolidated edge-predicate helpers and a `Cow` sort
  path.
- Cypher aggregation — `count`/`sum`/`avg`/`min`/`max`/`collect` (each with
  optional `DISTINCT`) with openCypher implicit grouping (Issue #558).
- Cypher `OPTIONAL MATCH` left-outer patterns (Issue #557).
- Cypher variable-depth traversal `-[:REL*min..max]->` (Issue #548).
- `USE` / `IN NAMESPACE` read-scope grammar in both the AQL and the Cypher
  parser, surfaced through the MCP `query` tool so a namespace can be selected
  in-query rather than only via a tool parameter (Issue #3349).

#### MCP / HTTP surface (Issues #3234, #3368, #3561, #3629, #3353, #3360)

- The MCP tool registry now exposes **74 tools**. The final wave (#3775)
  registered the ten tools whose Rust APIs had landed with their MCP surfaces
  deferred — `create_drift_monitor`, `list_drift_monitors`,
  `delete_drift_monitor`, `query_drift_alarms`, `resolve_drift_alarm` (#3367),
  `contradiction_genealogy`, `find_contradictions` (#3352),
  `counterfactual_replay` (#3357), and `trust_breakdown`,
  `list_trust_policies` (#3382) — and enrolled `get_belief_revisions` in the
  #3353 token budget. Each returns `FAILED_PRECONDITION` with
  `required_feature` when its experimental cohort is not compiled in.
- Structured error codes with a `retriable` flag and per-code `details`
  metadata (Issue #3234).
- Token-budget-aware responses: `max_response_tokens` / `max_response_bytes` /
  `priority_properties` on the budgetable read tools, degrading along a
  disclosed ladder with fetch handles (Issue #3353).
- Cursor continuation for large scans: snapshot-anchored, duplicate-free,
  gap-free paging on the bounded read tools (Issue #3360).
- Per-query resource limits (wall-clock timeout + result-byte cap) extended to
  the read tools, including a default-off memory-budget dimension (Issue #3368).
- Engine-lane per-query resource limits (Issue #3368): the executor now
  enforces limits **cooperatively** inside its pull-based iterator pipeline via
  a row-granular `ResourceGuardIterator` that aborts the scan rather than
  orphaning a background thread. Adds a public Rust builder API
  (`QueryBuilder::with_timeout`/`with_max_rows`/`with_memory_budget`),
  `AletheiaDBConfig::query_limits`, a structured
  `QueryError::ResourceExhausted { dimension, limit, consumed, retriable }`,
  and per-dimension counters via `AletheiaDB::query_limit_counters()`. The
  guard is skipped entirely under a fully-unlimited config, so the
  current-state/temporal hot paths (and the <1µs single-hop target) are
  unaffected.
- Inbound HTTP and MCP-over-HTTP concurrency budgets and body cap, rate-limit
  mounting, and timeout→429 mapping (Issue #3561).
- Constraint / precondition / conflict classification on the legacy JSON-RPC
  write path (Issue #3629/#3234).

#### Bi-temporal, provenance & lineage

- Valid-time writes on the convenience API and the MCP create/update/delete
  node/edge tools via an optional `valid_time` (Issue #3221).
- Valid-time retraction: `retract_node` / `retract_node_detach` /
  `retract_edge` close an entity's valid-time interval without deleting its
  history (Issue #3230).
- Queryable bi-temporal `temporal_extent` reporting the dataset's
  earliest/latest valid-time and transaction-time coordinates (Issue #3238).
- Derivation lineage: version-pinned upstream/downstream fact-to-fact closures
  (`create_*_with_lineage`, `upstream_lineage`/`downstream_lineage`, MCP
  `lineage_upstream`/`lineage_downstream`) (Issue #3371).
- Named snapshots for reproducible reads: pin a name to a bi-temporal
  coordinate whose handle returns identical results regardless of later writes
  (Issue #3370).
- Provenance-weighted retrieval fusion (Rust API + core) (Issue #3372).
- Belief-revision audit — when and why the database changed its mind
  (Issue #3362).
- Tamper-evident provenance hash chain with `aletheia verify` and the
  `verify_chain` / `export_chain_head` MCP tools (Issue #3351).
- Schema constraints — opt-in per-label/per-edge-type property types and
  required keys, enforced at the pre-apply commit hook (Issue #3378).

#### Batching & atomicity

- Atomic multi-write batches with local refs via MCP (Issue #3231): the new
  `apply_batch` tool accepts an **ordered** array of write operations
  (`create_node`, `create_edge`, `update_node`, `update_edge`, `delete_node`,
  `delete_edge`, each supporting the #3221 optional `valid_time`) that commit
  **all-or-nothing** in a single `WriteTransaction` (one WAL batch append,
  one GroupCommit fsync). A `create_node` may carry a `ref` alias; later edge
  operations may reference batch-created nodes as `"$alias"` or positionally
  as `"$<index>"` — forward/unknown/duplicate refs are rejected statically
  with a precise `details.failed_op_index` before any transaction opens. Any
  failure (validation, constraint violation, #3209 detach refusal — enforced
  against committed **and** batch-created edges via a batch-local adjacency
  ledger) rolls the whole batch back: zero writes become visible. On success
  the response returns per-operation results in input order (entity ids,
  version ids for creates/updates) plus a `ref_map` of every alias to its
  committed real id. Batch size is capped (default 1000, tunable via
  `AletheiaMcpServer::with_max_batch_operations`; the limit is echoed on
  rejection per #3226). See
  [docs/guides/mcp-query-tool.md](docs/guides/mcp-query-tool.md#atomic-multi-write-batches-apply_batch).

#### Authentication & RBAC

- Authentication and role-based access control on both server surfaces
  (Issue #3350): the HTTP server (`aletheia-server`) and the MCP server
  (`aletheia-mcp`) require an API key by default and refuse to start with
  zero credentials; anonymous operation is an explicit, loudly-warned
  opt-in (`ALETHEIADB_AUTH_MODE=anonymous`, fail-closed on invalid values).
  Four roles (`admin`/`writer`/`reader`/`metrics`) gate every HTTP route
  and MCP tool via classifications kept in lockstep with
  `docs/guides/access-control-matrix.md` by CI conformance tests. Key
  lifecycle is served by the HTTP `POST/GET /admin/keys` and
  `POST /admin/keys/revoke` endpoints over a persisted, hashed key store
  (`{data_dir}/auth/keys.json`, SHA-256 digests only, `0600`, atomic
  writes with directory fsync); credentials are re-verified per call so
  revocation is immediate. Auth failures are a uniform `UNAUTHENTICATED`;
  role denials are `PERMISSION_DENIED` — both additive to the #3234
  error-code enum. Authenticated writes stamp the verified principal's name
  into version provenance (`provenance.principal`) on the structured
  create/update node/edge paths of both surfaces. See
  [docs/guides/security-quickstart.md](docs/guides/security-quickstart.md).

#### Performance & indexing

- Secondary property (equality) index for `find_nodes_by_property` (Issue
  #3774): a `(label, property)` pair can be opted into a secondary index,
  replacing the O(nodes-per-label) scan-and-compare with a direct lookup. The
  namespace-scoped variant no longer scans then post-filters.

#### Durable workflows (DBOS phases, Issues #3755, #3759)

- Workflow journal schema convention with exactly-once step recording, so an
  agent running a multi-step process across a crash can tell which steps
  already happened instead of repeating a side effect (Phase 3a).
- A safe multi-executor fencing primitive: CAS/lease extensions that close the
  stale-fence collision where two executors stealing an expired lease could
  both stamp the same fence, plus `apply_batch` integration and the new
  `Error::FenceTooLow` (Phase 3e).

#### Tooling & platform

- Shell completions for bash/zsh/fish via `clap_complete` (Issue #3619).
- WebAssembly compatibility groundwork (Issues #3772, #3776): the seven
  non-optional wasm-hostile dependencies moved behind a
  `cfg(not(target_arch = "wasm32"))` target table (Phase 1), and the
  source-level use-sites of the vector and persistence subsystems gated so
  `cargo check` for `wasm32-unknown-unknown` core is green (Phase 2). No
  supported wasm build is shipped yet — this is dependency- and
  compile-layer only.
- LDBC-style benchmark suite with a self-hosted execution path and SF1 /
  large-vector scaling (Issue #3628, `crates/aletheia-bench-ldbc`).

#### Other

- Configurable string-interner cap `max_interned_strings` on
  `PersistenceConfig`, plus elimination of the background-persist infinite
  retry loop (Issue #3716).
- The #3218 unique-constraint registry is now included in `.albk` backups
  (Issue #3663).
- Runtime-installable keyring seams for the index tier (Issue #3708),
  completing the set alongside the merged WAL (#3669) and cold (#3733) seams,
  plus the hot-live `encryption enable` driver that flips a live plaintext
  instance to encrypted without reopening the database.

### Changed

- Vector index loading at startup is now parallel with per-index error
  isolation (Issue #451): with index persistence enabled, all per-property
  HNSW vector indexes load concurrently (one rayon task per property) and a
  corrupted or unreadable vector index is skipped with a warning instead of
  aborting the loading of every remaining index. A skipped index is recovered
  with `AletheiaDB::rebuild_vector_index(property, config)`. See
  [docs/guides/index-persistence-guide.md](docs/guides/index-persistence-guide.md#vector-index-persistence).
- Bulk MCP read responses now evaluate `is_current` against a single
  per-request timestamp (Issue #3391): the wallclock is captured once per
  tool call and every entity's `temporal.is_current` in that response
  (`list_nodes`, `traverse`, `get_outgoing_edges`/`get_incoming_edges`,
  `find_similar`, `find_nodes_at_time`, `hybrid_query`, ...) is judged against
  the same instant, instead of one clock read per serialized entity.
- Removed the legacy single-property temporal vector index state (Issue #450):
  the internal `TemporalVectorIndexState` (which mirrored only the most
  recently enabled temporal index) is gone, and the multi-property
  `temporal_vector_indexes` DashMap (Issue #389) is now the single source of
  truth. No public types were removed, but the property-less temporal APIs
  (`find_similar_as_of` and siblings) now deterministically query the
  **alphabetically first** temporal-indexed property instead of the most
  recently enabled one. Migration: name the property explicitly, e.g.
  `db.find_similar_as_of_in("content_embedding", &query, 10, ts)?`.
- Several breaking behavioral changes are cross-referenced under
  **Breaking changes** above (`PersistenceConfig::default()` no longer enables
  index persistence, #3388; `ReadOps` edge getters now return `Result`, #359).
- `aletheia-server` and the autumn migration spike moved to autumn-web 0.6.0
  (Issue #3761). Both are `publish = false` workspace members, so this does
  not affect the published `aletheiadb` crate.
- The TypeScript client (`@aletheiadb/client`, published separately on npm)
  tracks the unified error envelope and gained `priority_properties` support
  on `GET /schema` (Issue #3679).

### Fixed

- Replica reads could observe a partially-applied transaction (#3788): the
  asynchronous-replication applier selected whole `[BeginTx .. CommitTx]`
  frames, but the replay engine then applied the selected frame *operation by
  operation* into current-state storage, which reads do not lock. A read
  landing inside that window could see one node of a two-node commit — a
  referentially inconsistent view (a node whose sibling or edge endpoint did
  not exist yet) returned with no error. The `never torn` half of the
  replication consistency guarantee therefore did not hold for reads concurrent
  with an apply.

  A replica now arms a **current-state apply gate**; the applier publishes each
  batch inside a window, so a gated read sees the state from before the batch or
  after it, never inside it. Point lookups (`get_node`, `get_edge`, edge
  endpoint/label accessors, adjacency lookups, degrees) are atomic with respect
  to an apply. The gate is **disarmed on a primary** (one predictable branch, no
  lock), and is disarmed again after `promote_to_primary()` joins the applier;
  while armed it uses a seqlock — loads only, no atomic read-modify-write — so
  replica reader fan-out still scales across cores. Bulk scans and iterators are
  deliberately not gated (`DashMap` iteration was never a point-in-time snapshot
  even on a primary); use the bi-temporal reads for a true snapshot.

  New coverage: `torn_frame_safety_holds_under_sustained_concurrent_load`
  (`tests/replication_engine.rs`) holds the invariant under continuous apply with
  concurrent readers, and `cargo bench --bench replication_apply_gate` quantifies
  the read-path cost on both a primary and an idle replica. See
  `docs/guides/replication-guide.md` (*Reader isolation during apply*).

  `torn_frame_safety_never_exposes_a_partial_transaction`'s assertion was also
  corrected: it compared the two nodes' presence for *equality*, which flags the
  legitimate interleaving where the apply lands between the two reads (second
  node visible, first not yet read as present). The invariant it should encode —
  and now does — is one-directional: the *first*-written node visible with the
  second missing is the torn shape; the reverse is ordinary staleness.
- Multi-property temporal vector indexes now all receive write-path updates
  (Issue #450): with two or more temporal vector indexes enabled, node
  creates/updates index vectors into **every** matching property index,
  deletes remove the node from every index, and post-commit snapshot
  notifications reach every index. Previously only the most recently enabled
  temporal index was maintained.
- WAL: the flush coordinator no longer appends to an existing segment file
  whose header format version differs from the version the writer emits
  (Issue #3423). Replay derives the parse version solely from the segment
  header, so such an append produced a mixed-version segment whose newer
  entries failed CRC/parsing on recovery. The writer now rolls forward to the
  next segment id on a mismatched (or unreadable) header.
- `create_edge_with_valid_time` now enforces the same "not more than one year
  in the future" cap as every other `*_with_valid_time` operation; it
  previously accepted an arbitrarily-far-future `valid_time` on edges.
- The "valid_time must not precede entity creation" check on
  `update_node_with_valid_time`, `update_edge_with_valid_time`,
  `delete_node_with_valid_time`, and `delete_edge_with_valid_time` now
  compares against the entity's true original creation time instead of its
  most recent version, so backfilling a correction between two existing
  (already backdated) versions no longer fails with a spurious
  `ValidTimeBeforeEntityCreation` error.
- Backup restore no longer calls the process-global `GLOBAL_INTERNER.clear()`
  (Issue #3713), which could corrupt string labels in a concurrently-open
  database sharing the process.
- Recovery refuses a pre-v13 WAL tail instead of corrupting labels (Issue
  #3746). Opening a 0.1.x directory with an unreplayed pre-v13 tail previously
  *succeeded* while silently resolving labels to unrelated interned strings.
  See Breaking changes.
- Key rotation can be cancelled from any generation, not only `v1→v2` (Issue
  #3680). `cancel_pending_rotation` hard-coded the rollback target as the base
  key version, so cancelling an interrupted second (or n-th) rotation
  installed a keyring that could not decrypt the un-migrated files, leaving
  the dataset unrecoverable to its pre-rotation state. The old generation now
  comes from the pending ledger. A pending `enable`/`disable` encryption
  migration is additionally refused rather than driven through the reverse
  rotation pass.
- Cancelling a rotation that would leave the database **split-key** is now
  refused (Issue #3783). `cancel_pending_rotation` drives only the index
  reverse pass, but a full-MEK rotation also re-keys the WAL, cold tier, and
  subject keyring — none of which has a reverse pass. The cancel nevertheless
  cleared the ledger and reported success, so an operator told "the new key
  was never adopted" could discard a key that was still required, making
  new-DEK WAL segments, re-wrapped cold values, and wrapped per-subject DEKs
  permanently undecryptable. It now fails closed with
  `RotationError::UnsupportedCancelWithRekeyedLayers`, naming the offending
  layers and directing the operator to roll **forward** with
  `keys rotate --resume`; the pending ledger is left byte-identical and the
  refusal is audited as `key.rotation.failed`, never `completed`.
- PITR: the vocabulary-drift guard is WAL-version-aware (Issue #3745), so a
  legitimate point-in-time restore whose window crosses a post-backup
  vocabulary change (a new label or property key) succeeds instead of being
  refused with `WindowCrossesVocabularyChange`. A follow-up (Issue #3764)
  narrowed the guard from a whole-archive boolean to per-entry raw-label
  tagging, so a **mixed** archive (out-of-band ≤v12 segments plus a fully-v13
  replay window) restores instead of being conservatively false-rejected.

## [0.1.1] - 2026-05-12

### Fixed

- MCP server startup now uses `AletheiaDB::open_from_env()`, so stdio MCP
  sessions honor `ALETHEIADB_CONFIG` and `ALETHEIADB_DATA_DIR` instead of
  silently creating a fresh ephemeral database.

### Added

- Initial Python SDK package under `python/`, with PyO3 bindings for graph
  CRUD, traversal, temporal queries, vector search, and Cypher/AQL execution.
- Python wheel CI/release workflow for Linux, macOS, Windows, source
  distributions, and Trusted Publishing to PyPI on `python-v*` tags.

### Changed

- `AletheiaDB::new()` is now explicitly tempdir-backed and ephemeral; durable
  entry points should use `AletheiaDB::open_from_env()` or an explicit unified
  config.
- Updated the Python SDK's PyO3 dependency to `0.24`.
- Excluded `python/**` from the root Rust crate package published to crates.io.

## [0.1.0] - 2026-05-06

### Breaking

- **Experimental "Nova" feature split into category flags**
  ([ADR-0050](docs/adr/0050-experimental-feature-categorization.md)).
  The single `nova = []` flag has been replaced with five category flags:
  - `semantic-search` (graduated to **stable**)
  - `semantic-reasoning`
  - `semantic-temporal`
  - `semantic-diagnostics`
  - `semantic-characterization`

  The `nova` umbrella now enables only the four `semantic-*` cohorts. It **no longer
  enables the semantic-search cohort** — add `"semantic-search"` alongside
  `"nova"` in your `features` list to keep prior behaviour:
  ```toml
  aletheiadb = { version = "0.1", features = ["nova", "semantic-search"] }
  ```

- **Path change for graduated modules**: 14 search-cohort modules moved from
  `aletheiadb::experimental::*` to `aletheiadb::semantic_search::*`. Affected
  modules: `fishing`, `gestalt`, `cartographer`, `highlander`, `janus`,
  `chameleon`, `semantic_navigator`, `concept_algebra`, `serendipity`,
  `voyager`, `spectre`, `telepathy`, `tapestry`, `horizon`. Update imports:
  ```rust
  // Before
  use aletheiadb::experimental::fishing::FishingRod;
  // After
  use aletheiadb::semantic_search::fishing::FishingRod;
  ```

### Stabilized

- **Semantic search cohort graduates from experimental** to stable under the
  new `semantic-search` feature flag. Includes 14 modules covering associative
  retrieval, fuzzy pattern matching, clustering, entity resolution, and
  vector-guided traversal. The remaining "Nova" categories continue under
  `semantic-*` flags.

### Added

- `just check-features` recipe verifies each Nova/semantic-search category
  compiles standalone.

#### Phase 2: Hybrid Logical Clock Integration (2026-01-20)

- **Hybrid Logical Clock Timestamps** ([ADR-0024](docs/adr/0024-hybrid-logical-clock-timestamps.md))
  - Replaced simple `i64` timestamps with `HybridTimestamp` (12-byte structure)
  - Combines physical wallclock time (8 bytes) + logical counter (4 bytes)
  - Enables distributed operation with causal consistency
  - Provides strict total ordering for concurrent transactions
  - Maintains MVCC snapshot isolation guarantees
  - 25 new HLC-specific tests, all 1,327+ tests passing

**Breaking Changes:**
- `Timestamp` type alias now maps to `HybridTimestamp` instead of `i64`
- All timestamp parameters require `.into()` for integer literals
- Binary serialization format changed (8 bytes → 12 bytes)
- Arithmetic on timestamps requires wallclock accessor: `(offset + timestamp.wallclock()).into()`

**Migration Guide:**
```rust
// Before (Phase 1):
let timestamp: i64 = 1000;
let later = timestamp + 100;

// After (Phase 2):
let timestamp: Timestamp = 1000.into();
let later: Timestamp = (100 + timestamp.wallclock()).into();

// Or use the From trait:
use aletheiadb::core::temporal::Timestamp;
let timestamp = Timestamp::from(1000);
```

**Performance Impact:**
- Storage: +50% per timestamp (12 vs 8 bytes)
  - Mitigated by anchor+delta compression
  - Overall database overhead: <2%
- CPU: No measurable impact (comparison remains O(1))
- All performance targets maintained

**References:**
- PR #423: Phase 2 HLC Integration (299→0 compilation errors)
- [Logical Physical Clocks Paper](https://cse.buffalo.edu/tech-reports/2014-04.pdf) (Kulkarni & Demirbas, 2014)
- [CockroachDB HLC Blog Post](https://www.cockroachlabs.com/blog/living-without-atomic-clocks/)

---

#### Index Persistence Layer (2026-01-16)

- **Fast Cold Starts** ([ADR-0023](docs/adr/0023-index-persistence-layer.md))
  - Save indexes to disk for 6-30x faster startup
  - Zstd compression reduces disk usage by 60-75%
  - Memory-mapped loading for multi-GB indexes
  - Parallel loading (graph + temporal + vector)
  - Configurable via `PersistenceConfig`

**Performance:**
- 1M nodes: 30-60s WAL replay → 2-5s index loading
- 10M nodes: 5-10min WAL replay → 20-30s index loading
- Compression: ~65% size reduction with Zstd

---

#### Multi-Property Vector Indexing (2026-01-15)

- **Multiple Vector Properties** ([ADR-0022](docs/adr/0022-multi-property-vector-index.md))
  - Support multiple vector embeddings per database
  - Property-scoped vector indexes
  - Independent HNSW configurations per property
  - Temporal vector indexes with semantic drift tracking

**Use Cases:**
- Different embedding models (text vs image)
- Multi-lingual embeddings
- Domain-specific embeddings (code, documentation, data)

---

#### Hybrid Query System (2026-01-14)

- **Unified Query API** ([ADR-0021](docs/adr/0021-hybrid-query-execution.md))
  - Graph traversal + Vector similarity + Temporal queries
  - Builder pattern API
  - Query planner with cost-based optimization
  - Single query combining all three dimensions

**Example:**
```rust
db.query()
    .as_of(valid_time, tx_time)
    .start(alice_id)
    .traverse("KNOWS")
    .rank_by_similarity(&embedding, 10)
    .execute(&db)?;
```

---

#### Concurrent WAL Architecture (2026-01-10)

- **Striped Lock-Free WAL** ([ADR-0020](docs/adr/0020-concurrent-wal-architecture.md))
  - Lock-free ring buffer with 16 stripes
  - Configurable durability modes (Sync, GroupCommit, Async)
  - Background flush coordinator
  - Zero-allocation hot path

**Performance:**
- Sync: ~1.5ms latency, ~600 ops/sec
- GroupCommit: ~10-50ms latency, ~100K ops/sec
- Async: <100ns latency, ~500K ops/sec

---

#### Temporal Vector Search (2026-01-08)

- **Time-Travel Vector Queries** ([ADR-0017](docs/adr/0017-temporal-vector-strategy.md), [ADR-0018](docs/adr/0018-temporal-vector-historical-integration.md))
  - Snapshot-based temporal indexes
  - Point-in-time vector search
  - Semantic drift tracking
  - Integration with HistoricalStorage

**Use Cases:**
- "What was semantically similar in 2023?"
- "How has document meaning changed over time?"
- "Track knowledge evolution for LLM reasoning"

---

#### Embedding Providers (2026-01-04)

- **Pluggable Embedding Generation** ([ADR-0016](docs/adr/0016-embedding-providers.md))
  - OpenAI provider (text-embedding-3-small/large)
  - HuggingFace provider (local models)
  - Ollama provider (local LLMs)
  - ONNX provider (portable inference)
  - Feature flags for optional dependencies

---

### Changed

- **Storage Refactoring (Breaking Change)**: Removed the `ColdStorage` trait and `FileColdStorage` implementation.
  - `RedbColdStorage` is now the sole concrete implementation for cold storage.
  - `TieredStorage` and `MigrationService` now take `Arc<RedbColdStorage>` instead of `Arc<dyn ColdStorage>` or `Box<dyn ColdStorage>`.
  - Simplifies the storage hierarchy and removes dynamic dispatch overhead.
- Improved test coverage to 86.45% line coverage, 89.10% function coverage
- Enhanced CI/CD with automated benchmarking and coverage reporting
- Updated all documentation to reflect HybridTimestamp migration

### Fixed

- Doctest compilation issues in temporal vector examples
- HybridTimestamp deserialization validation for sentinel values
- Cleanup script and temporary file commits in repository

---

## Project Status

**Version:** 0.1.0
**Rust Version:** 1.92+
**License:** MIT OR Apache-2.0

**Test Coverage:**
- Library tests: 1,327 passing
- Doctests: 62 passing
- Property tests: Included
- Total: 1,400+ tests passing

**Performance** (historical averages across 30–212 CI datapoints):
- Node/edge lookup: 25.7 ns / 25.4 ns ✓ (target <1µs)
- Single-hop traversal: 185.8 ns ✓ (target <1µs)
- 3-hop traversal: 24.0 µs ✓ (target <100µs)
- Time-travel reconstruction: 82.8 ns ✓ (target <10ms)
- k-NN search k=10, 10K vectors: 127.2 µs ✓ (target <10ms)
- Graph + vector hybrid k=10: 22.5 µs ✓ (target <20ms)

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on contributing to AletheiaDB.

## References

- [Architecture Documentation](docs/ARCHITECTURE.md)
- [Architecture Decision Records](docs/adr/)
- [Testing Guide](TESTING.md)
- [Development Workflow](docs/DEVELOPMENT_WORKFLOW.md)
