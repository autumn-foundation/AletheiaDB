# Design sketch: AletheiaDB as a DBOS-style durable-execution store (Issue #3577)

Status: **design-only draft** · Base: origin/trunk @ 65b67f2 · Date: 2026-07-19 · Author: implementation agent

> DBOS-style durable execution needs three things from its backing store: a
> transactional **step journal** with exactly-once recording, **atomic claims**
> (executor recovery, work queues, durable timers), and **wakeups**
> (LISTEN/NOTIFY-style notification). AletheiaDB already ships primitives that
> compose cleanly into **two** of these — the **recording** half (`apply_batch` +
> uniqueness constraints) and the **notify** half (changefeed) — while the third,
> **safe multi-executor fencing**, needs a *small primitive extension* it does
> **not** have today (§5.3, OQ6/OQ8). Its bi-temporality additionally makes it
> *strictly more capable* than Postgres for the observability half. The
> compare-and-swap + lease write primitive lives in-tree at
> `src/api/transaction/write/cas.rs` (stamped **Issue #3577** in-source, though
> referred to as **#3604** elsewhere in the repo — see §3) and the changefeed /
> notify primitive at `subscribe_changes` / `list_changes`
> (`src/db/changefeed_sub.rs`, Issue #3375). This document designs the remaining
> **Phase 3**: a workflow-journal *schema convention* and a *reference executor
> loop* composed on those primitives — and is honest about the one place a
> convention over today's primitives is **not** sufficient (fencing). It is
> **design-only** — no source changes are proposed to land in this PR.

---

## 1. Problem & motivation

### 1.1 What DBOS-style durable execution is

A DBOS-style engine (Temporal/DBOS-Transact-shaped) runs application *workflows*
as ordinary code that survives process crashes. It does so by writing every
significant effect — each **step** (a.k.a. activity/transaction) — into a durable
**journal** *before* returning its result to the workflow. On restart or
failover, the engine **replays** the workflow function: steps whose result is
already in the journal are not re-executed; their recorded output is returned
directly (**memoization**). This is what makes execution "exactly-once" from the
workflow's point of view even though the process may have crashed and restarted
arbitrarily many times.

Replay only works if the workflow *orchestration* code is **deterministic**: the
memo is keyed by the step's **position** in the run, so replay must invoke the
same sequence of steps in the same order. This is a standard Temporal/DBOS
constraint and a hard requirement of this design (§2 N6), not something the store
can enforce.

Three store-level primitives make this work:

1. **Transactional step journal (exactly-once recording).** Recording "step N of
   workflow W produced output O" must be atomic and idempotent: a crash between
   *doing* the work and *recording* it, followed by a retry, must not create two
   journal entries for the same `(workflow, step)`.
2. **Atomic claims.** Exactly one executor may own a running workflow at a time.
   Claiming is a conditional write: *"take workflow W iff it is unclaimed or its
   lease has expired."* This one primitive underpins **crash recovery** (another
   executor steals an expired lease), **work queues** (the set of claimable
   pending runs), and **durable timers** (a scheduled wake-at coordinate plus a
   claimant). Safe multi-executor ownership *also* needs a **monotonic fencing
   token** to neutralise a zombie writer — which, as §5.3 shows, is exactly where
   today's primitive falls short.
3. **Wakeups.** Executors must not busy-poll. They need a LISTEN/NOTIFY-style
   signal — "a new run is claimable" / "this run's status changed" — to block
   until there is work.

### 1.2 Why AletheiaDB as the store

- **Durability core already present.** Acked-write-survives-crash is the WAL +
  group-commit contract (`src/storage/wal/durability.rs`). (A lost-write
  hardening is tracked in git history as Issue #3574 / PR #3573 — a process/PR
  reference, not a source-verifiable symbol in this worktree.) Point-in-time
  restore (`src/db/backup.rs`, plus the PITR path in `src/db/pitr.rs`,
  Issue #3374) covers disaster recovery.
- **Bi-temporality beats Postgres for the observability half.** Every fact
  AletheiaDB stores is a bi-temporal version (`VersionInfo`,
  `src/core/history.rs:45`) carrying both *valid time* (when the fact was true in
  reality) and *transaction time* (when it was recorded). A journal built on this
  substrate gives **native time-travel debugging of any past workflow run**:
  reconstruct the exact journal as it stood at any transaction-time coordinate
  (`AS OF SYSTEM_TIME`) with a single point-in-time read — no event-sourcing
  scaffolding, no audit-table bolt-on. With the tamper-evident provenance hash
  chain (`src/provenance_chain/`, Issue #3351) and principal provenance
  (Issue #3427), the journal is auditable and tamper-evident out of the box.
- **Embedded-first matches DBOS's SQLite mode.** `AletheiaDB::open(path)` is a
  one-line durable embedded store; the planned `aletheia serve` daemon is the
  scale-out executor shape.
- **One substrate for agent state + workflow state.** For an agentic system that
  already embeds AletheiaDB as its knowledge graph, workflow steps and knowledge
  facts share one store: *"what did the agent know when it took step N"* becomes a
  single `AS OF` query over the same database.

### 1.3 Scope of *this* document

The CAS/lease primitive (Issue #3577 in-source / referred to as #3604 elsewhere —
§3) and the changefeed/notify primitive (Issue #3375) both exist in-tree today.
(Note: #3577's own tracking comment still lists "changefeed/notify #3375" among
remaining-open phases; that comment predates #3375 landing, which is verified
present in-tree — `subscribe_changes`, `src/db/changefeed_sub.rs:55`.) This doc
does **not** re-propose these primitives; it treats them as available building
blocks (§3) and designs **Phase 3** on top: the journal schema convention
(§5.1–5.2), the claim/recovery/queue/timer mapping onto CAS+lease (§5.3), the
wakeup mapping onto the changefeed (§5.4), the time-travel / audit story (§5.5),
and a reference executor loop (§5.6). The central honest finding (§5.3): the
recording and notify halves compose on shipped primitives, but **safe
multi-executor fencing requires a small primitive extension** that today's
`claim_with_lease` does not provide.

---

## 2. Goals / Non-Goals

### Goals

- **G1.** Define a **workflow-journal schema convention** (node/edge shapes,
  property keys, constraints) expressible on today's AletheiaDB primitives.
- **G2.** Show **exactly-once step recording** composed from `apply_batch`
  (atomic multi-op) + a uniqueness constraint on the idempotency key.
- **G3.** Map **atomic claim / recovery / work-queue / durable-timer** semantics
  onto the shipped CAS + lease primitive (`claim_with_lease`), **and identify
  precisely where the fencing-token stale-write guard exceeds what the shipped
  primitive can enforce** (§5.3) — this is the design's key negative finding, not
  a footnote.
- **G4.** Map **executor wakeups** onto the shipped changefeed
  (`subscribe_changes` / `recv_timeout` / `await_changes`), with lossless
  recovery via `list_changes` + `resume_token`.
- **G5.** Show the **time-travel debugging + tamper-evident audit** story that
  differentiates AletheiaDB from Postgres.
- **G6.** Provide a **reference executor loop** in pseudocode plus a phased
  implementation outline and a risk→test map.

### Non-Goals

- **N1.** *Not* building a full DBOS engine here — no workflow runtime, no
  language-level `@workflow`/`@step` decorators, no scheduler daemon. This is a
  design sketch for the **store-side convention** and a reference loop.
- **N2.** **Exactly-once *recording* only.** The journal guarantees at-most-once
  *recording* of a step result; it does **not** make an external side-effect
  (HTTP POST, email, payment) exactly-once. Non-idempotent side effects still
  require caller-supplied idempotency keys and the standard "record-intent →
  perform → record-result" discipline (§8, risks #6/#17).
- **N3.** **No distributed executor consensus beyond leases.** Ownership is
  first-committer-wins CAS + time-based lease expiry, not Raft/Paxos. Note the
  clock-skew surface is **not** fully bounded by the DB: HLC drift handling
  (`src/core/hlc.rs`) bounds the DB's *internal* clock, but the lease deadline
  `lease_until` is computed on the *executor's* clock and passed in (§5.3), so a
  skewed-fast executor is a liveness hazard the DB clock does not bound (OQ8).
- **N4.** **No language SDKs / bindings** designed here.
- **N5.** **Not re-designing the CAS/lease or changefeed primitives** as they
  ship today; they are inputs. **However**, this design *does* identify a
  **required primitive extension** for safe fencing (§5.3, OQ6/OQ8) — that is a
  deliberate finding, not a redesign of existing surface.
- **N6.** **Deterministic workflow orchestration is assumed, not enforced.**
  Memoization keys on positional `(workflow_id, step_number)`. If the
  orchestration code *between* steps branches on wallclock/RNG/external state,
  replay can invoke a *different* step sequence, so `step_number = 3` on replay
  may be a different logical operation than the journaled step 3 — yet a naive
  memo read would return step 3's stored output (silent divergence). As in
  Temporal/DBOS, workflow orchestration code **MUST be deterministic**. The
  journal defends against a *mismatch* by asserting `memo.name == expected step
  name` and failing loudly (§5.2/§5.6), but cannot make nondeterministic
  orchestration correct (§8 risk #12).
- **N7.** **Throughput / write-amplification envelope not analyzed.** This sketch
  does not quantify per-step write amplification, memoize-read cost, or graph
  growth for long runs (§5.7). Benchmarks are deferred to Phase 3a prototyping
  (OQ9); CLAUDE.md mandates benchmarks for perf-critical features before they
  land, so this is an explicit deferral, not an omission.

---

## 3. Background: existing primitives we build on

Every claim below is cited to source in this worktree. Where a primitive only
*partially* fits the DBOS need, that is called out honestly.

| DBOS requirement | AletheiaDB primitive | Source (verified) | Fit |
|---|---|---|---|
| Durable, acked-write-survives-crash journal | WAL durability modes `Synchronous` / `Async` / `GroupCommit` / `AsyncBatched` | `src/storage/wal/durability.rs:52,66,82,130` | **Full.** `GroupCommit` (ACID, batched fsync) is the default journal mode; `Synchronous` for zero-loss; `Async` / `AsyncBatched` are *not* ACID (§9 open question on default). |
| Exactly-once step recording (atomic multi-op) | `apply_batch` — ordered ops, all-or-nothing, single `WriteTransaction` / one GroupCommit fsync | `src/mcp/batch.rs:1,415` | **Full** for the recording atom. **But** its op set is exactly six create/update/delete-node/edge variants (`BatchOperation`, `src/mcp/batch.rs:108`) — **no CAS/version-precondition op** (§5.3, OQ6). |
| "record iff idempotency key unused" | uniqueness constraint (`unique_constraint`) + schema constraints (#3378) | `src/db/ops.rs:1433`, `src/db/schema_constraint.rs:298` | **Full.** A duplicate `(workflow_id, step_number)` write fails at the pre-apply commit hook → `CONSTRAINT_VIOLATION`. Note: enforces one-record-per-key, **not** ownership (§5.3). |
| Atomic claim / lease | `claim_with_lease` / `claim_with_lease_with_options`; `compare_and_set_node` / `compare_and_set_edge` (+`_with_options`) | `src/db/ops.rs:728,762,666,695,707`; contract in `src/api/transaction/write/cas.rs` | **Full for single-winner claim.** Conditional **full-replace**; lease branch succeeds iff version matches OR `lease_until` (int micros) `<=` commit HLC. "Exactly one winner" under the commit-serialization guard. |
| Monotonic **fencing** token | *(none — must be composed by convention)* | see below | **Partial → not achievable by convention.** `claim_with_lease` stamps only caller-supplied `lease_owner`/`lease_until`; there is **no server-side fence increment and no fence precondition** (`claim_with_lease_impl`, `src/api/transaction/write/cas.rs:325`). Safe monotonic fencing needs a **primitive extension** (§5.3, OQ6/OQ8). |
| Wakeups (LISTEN/NOTIFY) | `subscribe_changes(ChangeFilter) -> Subscription`; `Subscription::poll()` / `recv_timeout(dur)` | `src/db/changefeed_sub.rs:55`; `src/core/changefeed_subscription.rs:396,433` | **Full** for push. Best-effort at-least-once; durable ground truth is pull `list_changes`. One event wakes *all* subscribers (thundering herd — §5.4). |
| Durable, replayable changefeed (lag recovery) | `list_changes(&ChangeFeedQuery) -> ChangeFeedPage`, `resume_token` / `ChangeCursor` dedup | `src/db/temporal.rs:562` | **Full.** Rebuilt by WAL recovery; lagged consumer resumes losslessly. |
| Time-travel debugging of past runs | Bi-temporal `VersionInfo`; `AS OF` valid/tx time reads | `src/core/history.rs:45` | **Full** — the differentiator vs Postgres. Caveat: cold-tier eviction / truncation can render a very old pinned version unreadable, but **replay** is unaffected (§5.5, §8 risk #10). |
| Fact-to-fact derivation (step → derived KG facts) | `LineageRef`, `create_node_with_lineage` / `create_edge_with_lineage`, upstream/downstream closure | `src/core/lineage.rs:53`, `src/db/lineage.rs:120,139` | **Partial.** v1 lineage index is **in-memory only** — it does *not* survive restart (`src/core/lineage.rs:34`). Journal correctness must **not** depend on it (§8 risk #7). |
| Tamper-evident audit | Provenance hash chain (#3351), principal provenance (#3427) | `src/provenance_chain/` (engine/record/store/verify) | **Full** for auditability; principal stamping covers structured create/update paths (deletes/retracts not yet — #3427). |
| Reproducible run views | Named snapshots (`create_snapshot`) | `src/db/snapshot.rs` | **Full** for pinning a run's KG view — but a snapshot is a **coordinate, not a held resource**; it pins no storage (§5.5, §8 risk #10). |
| Fast executor restart | Checkpoint + WAL replay | `src/storage/checkpoint.rs` | **Full.** <5 s medium-dataset recovery target (CLAUDE.md). |

**The lease primitive, precisely (verified).** `claim_with_lease(node_id,
expected_version, lease_owner_key, lease_until_key, owner, lease_until,
properties)` (`src/db/ops.rs:728`) stamps `lease_owner_key = owner` and
`lease_until_key = lease_until` (as integer micros) into a **full-replace**
property map, then commits under the `historical.write()` commit-serialization
guard. The claim succeeds iff `expected_version` matches **OR** the entity's
current `lease_until` property is `<=` the commit timestamp. Because claimants
write a **future** `lease_until`, at most one concurrent claim finds the lease
expired-or-matching → "exactly one winner." Lease comparison is at
**microsecond** granularity (`lease_until.wallclock()` drops the HLC logical
component; `src/api/transaction/write/cas.rs:321`). A loser gets a non-retriable
`TransactionError::CasMismatch` and writes nothing. **Crucially, the primitive
performs no server-side fence bookkeeping**: any `fence` property is just another
value the caller writes into the full-replace map, with no server-side increment
and no "supplied fence must exceed stored fence" precondition (verified
`claim_with_lease_impl`, `src/api/transaction/write/cas.rs:325`). §5.3 shows why
that makes convention-only fencing unsafe.

**A note on issue attribution.** The CAS/lease code is stamped **Issue #3577**
in-source (`src/api/transaction/write/cas.rs:2`, `src/db/ops.rs:754`
— "`claim_with_lease_with_options` (Issue #3577)"), yet `src/core/namespace.rs:11`
refers to the same primitive as "#3604 (CAS/lease)". This design doc is itself
#3577, so we do not assert a clean #3604-vs-#3577 split: the CAS/lease
**primitive exists in-tree** (cited above) regardless of which issue number owns
it, and that in-tree existence is all the design relies on.

---

## 4. Candidate architectures with trade-offs

Three shapes for where journal semantics live and how the journal is modeled.

### Approach A — Graph-native journal **(CHOSEN)**

`WorkflowRun` is a node; each `Step` is a node; edges link `run -[:HAS_STEP]->
step` and (optionally) `step -[:DERIVED]-> fact` via derivation lineage (#3371).
The journal is first-class graph data, queried by traversal and reconstructed by
`AS OF`.

- **Pros:** richest observability — a run and its steps are traversable; step
  outputs can link to the knowledge-graph facts they produced (lineage); *"what
  did the agent know at step N"* is one bi-temporal read on the same substrate;
  full time-travel debugging; provenance/hash-chain apply uniformly. Best
  exploits what makes AletheiaDB *not* Postgres.
- **Cons:** more schema-design surface (labels, edge types, property keys,
  constraints); traversal cost and unbounded graph growth for very long runs
  (§5.7); requires the executor to follow the schema convention faithfully.

### Approach B — Flat / KV journal

Steps are opaque property blobs keyed `(workflow_id, step_number)`, minimal or no
graph structure (a `Step` node per key, no run/step edges, no lineage).

- **Pros:** simplest; smallest schema; closest to a classic DBOS SQL table; least
  to get wrong in a v0.
- **Cons:** loses graph observability (no traversal from run to steps), loses the
  step→derived-fact lineage story, weaker "one substrate" narrative. Still gets
  bi-temporal time-travel (any node is versioned), so it keeps *most* of the
  Postgres-beating observability edge even without the graph shape.

### Approach C — External thin library over CAS+notify only

Treat AletheiaDB purely as a compare-and-set + notify store; the journal
semantics (memoization table, ordering) live entirely executor-side, AletheiaDB
just stores blobs the library interprets.

- **Pros:** least DB coupling; portable across stores.
- **Cons:** least native benefit — reinvents journal semantics the DB could
  express; forfeits graph + lineage + native audit; the bi-temporal edge is
  largely unused. Wrong altitude for a design whose thesis is "AletheiaDB is
  *more* capable than Postgres here."

### Decision

**Approach A (graph-native).** The entire motivation for AletheiaDB-as-DBOS-store
(Issue #3577) is the bi-temporal + graph observability edge over Postgres;
Approach C discards it and Approach B keeps only half. Approach A is chosen as the
target design, **with Approach B explicitly retained as a pragmatic v0 subset**:
the run/step *nodes* and the exactly-once recording (§5.1–5.2) are identical in
both; A simply *also* materializes the `HAS_STEP` / `DERIVED` edges. An
implementation can ship B first (nodes + constraints + claim/notify) and add the
edges/lineage in a later phase (§7, Phase 3d) without a data migration — the
edges are additive over the same nodes.

*(The pros/cons/decision analysis above is intentionally kept in one section; the
Design follows as §5.)*

---

## 5. Design (chosen: graph-native journal)

All shapes below are a **convention over existing primitives** — no new storage
format, no new WAL frame, no core API is proposed here — **except** the fencing
gap of §5.3, which is explicitly flagged as needing a primitive extension.

### 5.1 Workflow-journal schema convention

**`WorkflowRun` node** (label `WorkflowRun`):

| Property | Type | Meaning |
|---|---|---|
| `workflow_id` | String | Caller-supplied stable id; **unique constraint** on `(WorkflowRun, workflow_id)`. |
| `name` | String | Workflow function name / type. |
| `status` | String | `pending` / `running` / `completed` / `failed`. |
| `owner` | String | Current executor id holding the lease (empty/absent if unclaimed). |
| `lease_until` | Int (micros) | Lease deadline; the `lease_until_key` fed to `claim_with_lease`. **Computed on the executor's clock today** (§5.3, liveness caveat). |
| `fence` | Int | Intended monotonic **fencing token**, bumped on every successful claim — **but not soundly enforceable on today's primitive** (§5.3). |
| `input` | (blob) | Workflow input payload (see §8 risk #9 on size). |
| `created_at` | Int (micros) | Enqueue time (also the `valid_from` if backdated). |
| `wake_at` | Int (micros) | Durable-timer coordinate (absent unless timer-scheduled). |

**`Step` node** (label `Step`):

| Property | Type | Meaning |
|---|---|---|
| `workflow_id` | String | Owning run. |
| `step_number` | Int | Position in the run (the memo key axis; see N6 on determinism). |
| `idem_key` | String | `"{workflow_id}:{step_number}"`; **unique constraint** on `(Step, idem_key)` — the exactly-once guard. |
| `name` | String | Step function name — **also the replay-divergence guard** (§5.2 asserts the memo's `name` matches the expected step). |
| `status` | String | `completed` / `failed`. |
| `output` | (blob) | Memoized result returned on replay. |
| `error` | String | Error payload when `status = failed`. |

**Edges:** `WorkflowRun -[:HAS_STEP]-> Step` (run→step index for traversal);
optionally `Step -[:DERIVED]-> <fact node>` recorded via `create_edge_with_lineage`
/ `derived_from` (`src/db/lineage.rs:139`) so a step's knowledge-graph outputs are
version-pinned to exactly the fact versions it wrote.

**Constraints** (both enabled once at bootstrap, `src/db/ops.rs:1433` /
`src/db/schema_constraint.rs:298`): unique `(WorkflowRun, workflow_id)` and unique
`(Step, idem_key)`. These are enforced at the pre-apply commit hook for **all**
write paths including `apply_batch`, so one violating op aborts the whole
transaction with zero partial writes. **They enforce one-record-per-key, not
ownership** — a distinction that matters for fencing (§5.3).

### 5.2 Exactly-once step recording

Each step result is recorded in **one** `apply_batch` transaction
(`src/mcp/batch.rs:415`) containing: `create_node` for the `Step` (carrying
`idem_key` and `name`) and `create_edge` for `HAS_STEP` (referencing the step via
a batch `$alias`), all-or-nothing. Because `(Step, idem_key)` is unique, a
**retried** re-record of the same step hits `CONSTRAINT_VIOLATION` (#3234) and
writes nothing.

**Read-memoized-on-replay flow.** On (re)entering step N the executor first reads
the `Step` node for `idem_key = "{workflow_id}:{N}"`:

```
result = find Step where idem_key = "{W}:{N}"
if result exists:               # journal already has it (memoized)
    assert result.name == expected_step_name(N)   # else FAIL LOUD (N6): replay diverged
    if result.status == completed: return result.output   # skip re-execution
    else: propagate result.error
else:                           # first execution
    out = execute_step_N()      # the real (possibly non-idempotent) work
    try apply_batch([ create Step{idem_key, name, status:completed, output:out},
                      create edge HAS_STEP run->$step ])
    on CONSTRAINT_VIOLATION:    # a concurrent/duplicate executor won the race
        winner = re-read Step where idem_key = "{W}:{N}"
        assert winner.name == expected_step_name(N)   # else FAIL LOUD (N6)
        return winner.output    # adopt the winner's recorded result
    return out
```

The `CONSTRAINT_VIOLATION`→re-read branch makes recording **idempotent even under
a claim race** (two executors briefly both believing they own the run): only one
`Step` row can exist per `idem_key`; the loser adopts it. This is *recording*
exactly-once, not *side-effect* exactly-once (N2).

The `assert result.name == expected_step_name(N)` guard turns a
nondeterministic-orchestration divergence (N6) into a **loud** failure rather than
silently returning position N's stored output for what is now a *different*
logical operation at position N — because the memo keys on positional
`step_number`, not on the step's identity (§8 risk #12). It cannot *repair*
nondeterminism; it only refuses to compound it.

### 5.3 Atomic claim / recovery / queues / timers (CAS + lease)

**Claim.** An executor claims a `WorkflowRun` node by
`claim_with_lease(run_node, expected_version, "owner", "lease_until", owner_id,
lease_until, new_props)` (`src/db/ops.rs:728`). The full-replace `new_props`
set `status = running` and `owner = owner_id`. The call wins iff the version
matches or the stored `lease_until <= commit HLC` — so a `pending` run (never
claimed) *or* a run whose lease expired is claimable, and a `running` run with a
live lease is not.

**Crash recovery.** No special path: a crashed executor simply stops renewing.
When its `lease_until` passes, *any* executor's `claim_with_lease` finds the lease
expired and steals the run. First-committer-wins under the commit guard guarantees
exactly one successor *claim*.

**Work queue.** The queue is a *query*, not a structure: the set of claimable runs
= `find_nodes` where `label = WorkflowRun AND status = pending` **plus** running
runs whose `lease_until < now` (expired). An executor pulls a candidate and
attempts to claim; losers move to the next candidate. (Fairness/ordering and the
thundering-herd interaction with §5.4 are executor policy — §9 OQ7.)

**Durable timer.** A scheduled wake is a `WorkflowRun` (or a dedicated `Timer`
node) with `status = pending` and `wake_at = T`. It is *not claimable* until
`now >= wake_at` (executors filter the queue query by `wake_at <= now`). Firing =
claiming it via the same lease path. Persistence of the coordinate is free
(`wake_at` is a property surviving crash/restart), **but firing is not instant at
`T`**: nothing fires the timer *at* `wake_at`; the run merely becomes claimable at
`T`, and is actually claimed only when some executor's queue query next runs — at
worst `T + idle_backoff` (§5.4). "Durable timers for free" carries that precision
bound (§8 risk #13).

**Lease deadline is on the executor's clock (liveness hazard).** `lease_until =
now() + TTL` is computed on the *executor's* wallclock and passed into the claim;
the DB stores it verbatim. A skewed-*fast* executor writes a far-future
`lease_until`, so if it then crashes the run stays un-stealable for a long time —
a **liveness** hole (not a safety one). HLC drift handling bounds the DB's
*internal* clock, not this caller-supplied value (this corrects N3's coverage
claim). Mitigation, both needing a small primitive change: have the DB compute
`lease_until = commit_HLC + server-side TTL` (caller passes only the TTL), or
clamp a caller-supplied `lease_until` to `commit_HLC + max_ttl` (OQ8).

**Fencing (stale-write guard) — and why it needs a primitive extension.** The
`fence` token is meant to defend against a *zombie* executor — one paused (GC, VM
stall) past its lease, whose run was stolen, that then wakes and tries to write.
The natural convention is to compute `fence = old_fence + 1` from the fence value
read in the queue query and stamp it into the claim's full-replace map. **This is
not sound on the shipped primitive**, for a compounding pair of reasons:

1. **No server-side fence.** `claim_with_lease` stamps only the caller-supplied
   `owner`/`lease_until`; it performs **no** server-side fence increment and
   enforces **no** "supplied fence must exceed stored fence" precondition
   (`claim_with_lease_impl`, `src/api/transaction/write/cas.rs:325`). The `fence`
   is just another property the caller writes.
2. **The lease-steal path reads a stale fence.** The steal branch wins via
   `lease_until <= commit HLC` — *without* a version match. So the `old_fence` a
   caller read in the (earlier, un-serialized) queue query can be stale by commit
   time: two executors that both observed `old_fence = 7` both compute and stamp
   `fence = 8`. They get the **same** fence — defeating fencing in exactly the
   recovery scenario it exists for.

**Consequence:** safe monotonic fencing is a **required primitive extension**, not
something achievable by convention on today's primitives. Either (a) the claim
must increment the fence **server-side, atomically inside the claim's
commit-guarded critical section**, or (b) `claim_with_lease` must gain a
precondition "the supplied fence is strictly greater than the stored fence, else
abort." The shipped #3577/#3604 primitive does **neither** (§8 risk #4, OQ6, OQ8).
Until it does, the fence arithmetic in the reference loop (§5.6) is illustrative of
the *intended* guard, not a sound one. This is the precise sense in which the
recording/notify halves compose on shipped primitives while **fencing does not**.

**A second, independent hole: fencing the record *content*.** Even granting a
sound monotonic fence, the step-record write must itself be gated on it, or a
zombie that lost its lease can still land a `create Step{output}` — the
`(Step, idem_key)` unique constraint enforces one-record-per-key but **not**
ownership, so the zombie can *poison* the memoized output a live successor will
later replay. Gating the record atomically requires a CAS/version-precondition
**inside** the same `apply_batch` as the `create Step`. **`apply_batch` has no
such op** — OQ6 below is now a **resolved-negative finding**: `BatchOperation`
(`src/mcp/batch.rs:108`) is exactly the six variants CreateNode / CreateEdge /
UpdateNode / UpdateEdge / DeleteNode / DeleteEdge — no CAS/claim/compare-and-set
op. The fallback — a separate `compare_and_set_node` on the run's fence in its own
transaction *before* the record `apply_batch` — is **not** fully safe: the record
transaction is still unfenced, so a zombie that passed a stale-fence check
(hole #1) or that races between the fence-CAS and the record can still write the
poisoning `Step`. This is **independent of hole #1**: even with a sound
CAS-in-batch op, the stale-fence collision remains. Fencing record content
therefore needs **either** an `apply_batch` CAS/version-precondition op (a real
follow-up — an extension of #3231, **not** an "implementation detail") **or**
per-op expected-version preconditions in `apply_batch` (also absent today). The
guarantee §5.2 offers is thus *exactly-once recording under a benign claim race*,
**not** *fenced-against-a-malicious-zombie recording* — the latter awaits the
primitive extension.

### 5.4 Wakeups (changefeed #3375)

Executors block instead of polling. Each subscribes:
`subscribe_changes(ChangeFilter::all().with_node_labels(["WorkflowRun"]).with_change_types([Created, Updated]))`
(`src/db/changefeed_sub.rs:55`; filter builders at
`src/core/changefeed_subscription.rs:189+` — fields `node_labels`, `edge_types`,
`change_types`, `namespace`; unset dimension = match-all). The returned
`Subscription` exposes `poll()` (non-blocking drain) and `recv_timeout(dur)`
(Mutex+Condvar long-poll — the LISTEN/NOTIFY wakeup;
`src/core/changefeed_subscription.rs:433`). A new `pending` run (Created) or a
status change (Updated) wakes a waiting executor, which then runs the queue query
(§5.3) and attempts a claim.

**Thundering herd.** One `Created` event wakes *all* subscribers (up to the
128-subscription changefeed cap); each runs the queue query and attempts the
claim, and N−1 eat a `CasMismatch`. For small executor pools this is fine; at
scale, add jittered backoff or a work-stealing hand-off so a single new run does
not stampede every executor (§8 risk #14, OQ7).

**Lag / overflow recovery.** The push feed is **best-effort at-least-once**. A
lagged consumer (bounded buffer overflow → disconnect) resumes **losslessly** by
pulling `list_changes` (`src/db/temporal.rs:562`) from its last `resume_token`
(a `ChangeCursor`), deduping by the stable `(tx_time, kind, entity_id,
version_id)` key. So the wakeup is an *optimization* over polling; correctness
never depends on a notification arriving — the durable `list_changes` pull is the
ground truth. (MCP surface: `await_changes` long-poll, deliberately excluded from
the #3368/#3353/#3360 wrappers since a long-poll is expected to block.)

### 5.5 Time-travel debugging & tamper-evident audit

- **Time-travel.** Because every `WorkflowRun`/`Step` write is a bi-temporal
  version (`src/core/history.rs:45`), `AS OF SYSTEM_TIME <t>` reconstructs the
  **exact journal** as it stood at transaction-time `t`: replay or inspect any
  historical run at any past instant, see a step's memoized output as first
  recorded even after later corrections, and answer *"what did the agent know when
  it took step N"* by reading the shared KG at that same coordinate. This is the
  concrete Postgres-beating differentiator (§1.2).
- **Replay correctness vs cold eviction — an important distinction (a genuine
  strength).** The *memoization* reads the executor loop performs (§5.2) read
  **current-state** `Step` nodes, which are append-only and never superseded (the
  journal only ever *appends* step rows — never delete/update a step). Those stay
  hot and are **not** threatened by cold-tier eviction or truncation, so *replay
  correctness is unaffected by retention policy*. Only historical `AS OF`
  *debugging* of superseded/old versions is bounded by cold eviction — and even
  then reconstructing a very old run requires anchoring **both** valid and
  transaction time before any superseding write (same caveat `temporal_extent`
  documents). Do not conflate "old run is un-inspectable via `AS OF`" with "replay
  is broken"; only the former can happen (§8 risk #10).
- **Audit / tamper-evidence.** Principal provenance (#3427) stamps the executor
  principal into version provenance on structured create/update paths; the
  provenance **hash chain** (`src/provenance_chain/`, #3351) makes the journal
  tamper-evident — `aletheia verify` / the `verify_chain` MCP tool detect any
  retroactive edit. Honest gap: deletes/retracts do not yet stamp a principal
  (#3427), so a journal that only ever *appends* step rows (recommended — never
  delete a step) stays fully attributed.

### 5.6 Reference executor loop (pseudocode)

The fence arithmetic below is written as the *intended* guard; it is **not sound**
on today's `claim_with_lease` (§5.3) and is annotated as such. Every `apply_batch`
is audited against the "one write per committed entity per batch" limit
(CLAUDE.md) and the "no CAS op in `apply_batch`" finding (§5.3).

```
loop:
    # 1. Wait for work without polling
    subscription.recv_timeout(idle_backoff)         # §5.4 wakeup

    # 2. Find & claim a run (queue = a query, §5.3)
    for run in find WorkflowRun where
              status == 'pending' OR lease_until < now(),
              and (wake_at is absent OR wake_at <= now()):   # durable timers
        try:
            # NOTE: fence:run.fence+1 is the INTENDED guard but is NOT sound on
            # today's claim_with_lease — the fence read here can be stale and the
            # steal path does not re-check it, so two stealers can stamp the SAME
            # fence (§5.3, OQ6/OQ8). Sound fencing needs a server-side atomic
            # fence increment / precondition.
            v = claim_with_lease(run, run.version,
                                 owner=me, lease_until=now()+LEASE_TTL,  # executor clock! (§5.3)
                                 props={status:'running', owner:me,
                                        fence:run.fence+1})
            my_fence, my_run_version = run.fence + 1, v
        except CasMismatch:
            continue          # someone else won; try next candidate
        break
    else:
        continue              # nothing claimable; loop back to wait

    # 3. Execute steps with journal memoization (§5.2)
    spawn lease_renewer(run, me):        # periodically re-claim to extend lease
        every LEASE_TTL/3:
            my_run_version = claim_with_lease(run, my_run_version,
                                 lease_until=now()+LEASE_TTL,
                                 props={..., fence:my_fence})   # keep same fence

    for step_number in workflow.steps:
        memo = find Step where idem_key == "{run.workflow_id}:{step_number}"
        if memo exists:
            assert memo.name == step_name(step_number)   # else FAIL LOUD (N6, §5.2)
            result = memo.output                          # skip re-execution (replay)
        else:
            out = execute(step_number)                    # real work (§8 risks #6/#17)
            try:
                # INTENDED: fence this record so a zombie that lost the lease cannot
                # poison the Step. NOT expressible today: apply_batch has NO CAS op
                # (OQ6 resolved-negative), and even a separate pre-record CAS leaves
                # this create unfenced (§5.3, §8 risk #4). Shown unfenced, honestly.
                apply_batch([
                    create Step{idem_key, name, status:'completed', output:out},
                    create edge (run)-[:HAS_STEP]->($step),
                ])
            except ConstraintViolation:                   # duplicate record
                winner = re-read Step{idem_key}
                assert winner.name == step_name(step_number)
                result = winner.output                    # adopt winner (§5.2)
            else:
                result = out

    # 4. Mark run done — a SINGLE fenced full-replace, not a 2-op batch.
    #    apply_batch forbids two writes to one committed entity per batch AND has
    #    no CAS op, so the earlier "CAS run + update run" pair is invalid; the
    #    full-replace compare_and_set_node does exactly what that pair intended.
    compare_and_set_node(run, my_run_version,
                         props={status:'completed', owner:'', lease_until:0,
                                fence:my_fence})
    stop lease_renewer

# On crash anywhere above: the lease simply stops renewing; when lease_until
# passes, another executor claims (step 2) and resumes from the journal (step 3)
# — completed steps are memoized, so no step re-executes. (A zombie that wakes
# after losing its lease is NOT reliably fenced today — §5.3.)
```

### 5.7 Performance & write-amplification (not analyzed here)

Called out honestly and deferred (N7, OQ9): each recorded step is a
`create_node` + `create_edge` in its **own** `apply_batch` — i.e. one
GroupCommit fsync per step — plus a per-step read-memoize lookup, and, under
Approach A (graph-native), **unbounded graph growth per long-running workflow**
(steps + `HAS_STEP` + `DERIVED` edges accumulate). None of throughput, per-step
write amplification, memoize-read cost, or long-run traversal cost is quantified
in this sketch. Peer plan docs carry perf targets and CLAUDE.md mandates
benchmarks for perf-critical features before they land, so this is an explicit
Phase 3a prototyping deferral (OQ9), not an omission. A likely mitigation to
prototype: batch several steps' records per fsync where the workflow permits, and
cap/prune or cold-migrate very old completed runs (§8 risk #10 interaction).

---

## 6. Brainstorm / Reverse-brainstorm / Six hats

### 6.1 Brainstorm (green hat) — capabilities this unlocks

- Native **time-travel replay** of any past run (`AS OF`) — reproduce a
  production incident by reading the journal as it was at the failing instant.
- **One substrate** for agent knowledge + agent workflow — "what did I know at
  step N" is a single bi-temporal read, no cross-store join.
- **Tamper-evident, attributed** step history (hash chain + principal) — audit
  without an external audit log.
- **Derivation lineage** from a step to the exact KG fact versions it produced
  (`Step -[:DERIVED]-> fact`, version-pinned) — blast-radius analysis of a bad
  step.
- **Durable timer coordinates for free** — a `wake_at` property, no separate
  scheduler store (with the firing-latency bound of §5.3).
- **Backup/restore + named snapshots** give reproducible whole-engine
  point-in-time restore and pinned run views with no new machinery.

### 6.2 Reverse-brainstorm — how could this design fail / corrupt / mislead?

| Hazard | Consequence | Mitigation |
|---|---|---|
| **Non-deterministic step *body*** (a step reads wallclock / RNG / external state that changed) | Replay produces a different value than the memoized one → divergent workflow | Memoize **outputs**, never re-derive on replay; the journal's recorded `output` is authoritative. Side effects go *inside* a step; results are journaled (N2). |
| **Non-deterministic *orchestration*** (branch between steps changes the step sequence) | Positional `step_number` memo returns a *foreign* step's output at position N → silent divergence | Require deterministic orchestration (N6); the memo-hit path **asserts `memo.name == expected step name` and fails loud** (§5.2), never returning a foreign output. |
| **Lease race / zombie writer** | Two executors both write the journal → corruption | Intended fix is a fencing token — but it is **not soundly enforceable on today's primitive**: no server-side fence + stale-fence steal collision (§5.3). Safe fencing is a **required primitive extension** (OQ6/OQ8); until then a zombie's record is not reliably fenced. |
| **Notify loss / lag** | Executor sleeps through claimable work | Wakeup is best-effort; the durable `list_changes` pull + `resume_token` is ground truth; executors also wake on `recv_timeout` backoff and re-run the queue query. |
| **Thundering herd on one event** | One `Created` wakes all ≤128 subscribers; N−1 waste a claim attempt | Jittered backoff / work-stealing hand-off (§5.4, OQ7). |
| **In-memory lineage lost on restart** (`src/core/lineage.rs:34`) | `DERIVED` closure empty after crash → misleads a blast-radius query | Journal **correctness must not depend on lineage**; `HAS_STEP` edges (durable graph edges) carry run→step structure. Lineage is observability-only until #3413 makes it durable (§8 risk #7). |
| **Unbounded run retention vs cold-tier eviction** | Old runs migrate to cold / get truncated → `AS OF` debugging of ancient runs fails | **Replay is unaffected** (memoize reads hot current-state append-only `Step` nodes — §5.5). For runs that must stay *AS-OF-inspectable*, use `backup` (a whole-engine artifact) or an explicit policy exempting journal data from cold migration / WAL truncation until a retention horizon. A named snapshot does **not** help — it is a coordinate, pins no storage, and an evicted version stays unreadable through it (§8 risk #10, OQ3). |
| **Step payload too large vs token/response budget** | Big `output`/`input` blobs bloat reads, blow MCP token budgets | Store large payloads by reference (hash → blob store) or accept vector/large-value elision (#3220/#3353); constrain step output size (§8 risk #9). |
| **Backdated `valid_time` reorders the journal** | A step recorded with a past `valid_from` looks out-of-order vs `step_number` | Journal **ordering is `step_number` (a property), never valid-time**; `valid_time` is metadata only (§8 risk #8). |
| **`Async` / `AsyncBatched` WAL mode used for the journal** | Acked step "recorded" then lost on crash (neither is ACID) | Default the journal to `GroupCommit`/`Synchronous`; forbid non-ACID modes for the journal store (§9 OQ2). |

### 6.3 Six hats (condensed)

- **White (facts):** CAS/lease (§3, stamped #3577 / referred to as #3604) and
  changefeed (#3375) exist in-tree and are cited; `apply_batch` + unique
  constraints give the recording atom; bi-temporal versions give time-travel.
  `apply_batch` has **no CAS op** and `claim_with_lease` has **no server-side
  fence** (both verified). Lineage index is in-memory (verified).
- **Red (gut):** the graph-native journal *feels* right — it is the one design
  that uses what makes AletheiaDB different; a KV-only journal feels like using a
  graph DB as a worse Postgres.
- **Black (risks):** the fencing-primitive gap (§5.3) is the sharpest — safe
  multi-executor fencing is *not* free on today's primitives. Then:
  orchestration determinism, executor-clock lease liveness, notify loss, thundering
  herd, cold-tier eviction of old runs, payload size, in-memory lineage,
  write-amplification — each has a named mitigation (§6.2) and most a test (§8).
- **Yellow (upside):** native time-travel debugging + tamper-evident audit +
  single-substrate agent state is a real, hard-to-replicate advantage over
  Postgres-backed DBOS.
- **Green (alternatives):** ship Approach B (KV journal) as v0, add graph
  edges/lineage (Approach A) additively; the engine could live as a library, an
  autumn plugin, or in-core (§9 OQ1).
- **Blue (process / next step):** this is a design sketch; next is coordinator
  review of the open questions (§9) — especially the fencing primitive extension
  (OQ6/OQ8) — then Phase 3a (schema + idempotent recording) behind the shipped
  primitives.

---

## 7. Not-in-scope recap & phased implementation outline

**Not in scope** (see §2 Non-Goals): full engine runtime, exactly-once external
side effects, executor consensus beyond leases, language SDKs. **In scope as a
finding** (not a redesign): the fencing primitive extension of §5.3.

**Already in-tree:** the CAS/lease primitive (Issue #3577 in-source / #3604
elsewhere) and the changefeed/notify primitive (#3375). The remaining work is
Phase 3, sub-phased:

| Phase | Deliverable | Rough size |
|---|---|---|
| **3a** | Journal schema convention + idempotent step recording: `WorkflowRun`/`Step` labels, the two unique constraints, the `apply_batch` record + `CONSTRAINT_VIOLATION`→re-read flow + `name` divergence assert (§5.1–5.2). Equivalent to Approach B v0. Includes the deferred perf/write-amplification benchmarks (N7, OQ9). | **M** |
| **3b** | Reference executor loop: claim → memoized-step loop → lease renewer → completion (§5.6). Library/plugin, not core. **Depends on 3e for *safe* fencing** — without it, 3b is single-executor-safe / benign-race-safe only. | **M** |
| **3c** | Durable timers + work-queue query helpers + wakeup wiring (`subscribe_changes`/`recv_timeout`, lag recovery via `list_changes`), incl. thundering-herd backoff (§5.3–5.4). | **S–M** |
| **3d** | Graph-native observability (Approach A full): `HAS_STEP` / `DERIVED` edges + lineage, and a **time-travel run inspector** (`AS OF` journal reconstruction, hash-chain verify) (§5.5). | **L** |
| **3e** | **Primitive extension for safe fencing** (§5.3, OQ6/OQ8): server-side monotonic fence increment / fence precondition on the claim, and DB-computed / clamped `lease_until`. A core change (extends the CAS/lease primitive + optionally `apply_batch` op set) — the prerequisite for multi-executor safety. | **M** |

Phases 3a/3c/3d are shippable on today's primitives; 3d layers observability
additively over the same nodes (no migration). **3b is only *safe* against zombie
writers once 3e lands** — this is the honest gating dependency.

---

## 8. Risks / edge-cases as test cases

| # | Scenario | Expected behavior | Test to write |
|---|---|---|---|
| 1 | **Duplicate step record under retry** | Second `apply_batch` for same `idem_key` → `CONSTRAINT_VIOLATION`; executor re-reads and returns the memoized output; exactly one `Step` node exists | `dup_step_record_is_idempotent` |
| 2 | **Executor crash mid-step** (work done, not yet recorded) | On recovery the step is *not* in the journal → successor re-executes it once; no double *record* | `crash_before_record_reexecutes_once` |
| 3 | **Two executors race to claim** one `pending` run | Exactly one `claim_with_lease` wins; loser gets `CasMismatch`, tries next candidate | `concurrent_claim_single_winner` |
| 4 | **Lease expiry during a long step** (zombie) | Successor steals the run; zombie's later write **should** be fenced out — but on today's primitive it is **not reliably** (no server-side fence; stale-fence collision, §5.3). Document the gap and gate the safety claim on the 3e extension | `zombie_write_not_reliably_fenced_today` (documents the gap) |
| 5 | **Notify overflow / lag** | Subscription disconnects; executor resumes from `list_changes` at last `resume_token`, dedups by `ChangeCursor`; no missed run | `notify_lag_recovers_via_list_changes` |
| 6 | **Non-deterministic step *body* on replay** | Replay returns the **memoized** output, does not re-run the step; value is stable across replays | `replay_returns_memoized_not_recomputed` |
| 7 | **Restart loses in-memory lineage** (`src/core/lineage.rs:34`) | `HAS_STEP` graph edges + journal reads still fully reconstruct the run; only the `DERIVED` lineage closure is empty (observability-only, not correctness) | `journal_intact_after_lineage_reset` |
| 8 | **Backdated `valid_time` vs journal ordering** | Replay order follows `step_number`, not `valid_from`; a backdated record does not reorder steps | `backdated_valid_time_preserves_step_order` |
| 9 | **Step payload too large** | Oversize `output` handled by reference/elision; read stays within response/token budget; no unbounded blob in a read | `large_step_payload_bounded` |
| 10 | **`AS OF` debug of a cold-evicted old run** | `AS OF` reconstruction past the cold horizon fails cleanly (documented), **but replay of that run still works** (memoize reads hot current-state `Step` nodes, §5.5) | `as_of_beyond_cold_horizon_errors_but_replay_ok` |
| 11 | **`Async`/`AsyncBatched` WAL mode misconfigured for journal** | Config guard rejects/warns; journal defaults to `GroupCommit`/`Synchronous` | `journal_rejects_non_acid_wal` |
| 12 | **Non-deterministic *orchestration*** (replay invokes a different op at position N) | Memo-hit path asserts `memo.name == expected step name` and **fails loud**, never returning the foreign output (N6) | `orchestration_divergence_fails_loud_on_name_mismatch` |
| 13 | **Durable-timer firing latency** | A timer with `wake_at = T` fires no earlier than `T` and no later than `T + idle_backoff` (queue-query cadence), not exactly at `T` | `durable_timer_fires_within_backoff_bound` |
| 14 | **Thundering herd on one `Created` event** | All subscribers wake and attempt the claim; exactly one wins, the rest `CasMismatch`; backoff/work-stealing bounds wasted attempts | `single_event_stampede_one_winner` |
| 15 | **Poison run** (a deterministically-failing run) | Without a cap, the run is re-claimed forever after each lease lapse; a `max_attempts` / dead-letter policy must stop the loop | `poison_run_dead_letters_after_max_attempts` |
| 16 | **Stale-fence collision** (two stealers compute the same fence) | On today's primitive both can stamp `fence = N` (no server-side increment / precondition) — test asserts the collision is *possible today*, gating the safe-fencing claim on 3e | `stale_fence_collision_possible_without_primitive_extension` |
| 17 | **Concurrent divergence under a claim race** | Both executors execute the step; their non-deterministic outputs may differ; the constraint keeps one *record*, but the **loser has already performed its side effect** with a divergent value the journal never reflects (sharper N2) | `claim_race_loser_side_effect_diverges` |

---

## 9. Open questions for the user

- **OQ1 — Where does the workflow engine live?** A standalone Rust library, an
  autumn-side plugin (matching the `aletheia serve` daemon plan), or in-core? The
  schema convention is store-agnostic; the executor loop is not.
- **OQ2 — Default WAL durability mode for the journal?** `GroupCommit` (ACID,
  high throughput) is the natural default; `Synchronous` for zero-loss at lower
  throughput. `Async`/`AsyncBatched` are *not* ACID
  (`src/storage/wal/durability.rs`) and must be forbidden for the journal —
  confirm.
- **OQ3 — Run retention vs cold-tier eviction.** Time-travel *debugging* of old
  runs conflicts with cold-tier migration/truncation (replay itself is
  unaffected — §5.5). What is the retention policy: `backup` runs that must stay
  AS-OF-inspectable, or an explicit policy exempting journal data from cold
  migration / WAL truncation until a retention horizon? (Named snapshots do
  **not** help — a snapshot is a coordinate, not a held resource.)
- **OQ4 — Exactly-once external side-effects: in or out of scope?** This design
  guarantees exactly-once *recording* only (N2). Is a side-effect idempotency-key
  convention (record-intent → perform → record-result) wanted in Phase 3, or left
  to callers? (See the sharper concurrent-divergence form, §8 risk #17.)
- **OQ5 — Multi-executor fencing correctness proof.** Do you want a written
  argument (or model-checked spec) that fencing + first-committer-wins CAS admits
  no two-writer window under HLC clock skew? **Note this is only meaningful once
  the OQ6/OQ8 primitive extension lands** — today the property does not hold
  (§5.3, §8 risk #16).
- **OQ6 — CAS-op inside `apply_batch` (RESOLVED-NEGATIVE).** The fencing design
  (§5.3, §5.6) wants a `compare_and_set`/version-precondition op composed *inside*
  an `apply_batch` alongside `create` ops. **This does not exist today:**
  `BatchOperation` (`src/mcp/batch.rs:108`) is exactly six variants — CreateNode /
  CreateEdge / UpdateNode / UpdateEdge / DeleteNode / DeleteEdge — with **no**
  CAS/claim/compare-and-set op. So fencing the *record content* is **not**
  expressible on today's `apply_batch`; the separate-CAS-before-record fallback
  leaves the record itself unfenced (§5.3). Adding such an op (an extension of
  #3231) or per-op expected-version preconditions is a **real follow-up**, not an
  implementation detail. Do you want this bundled into the 3e primitive-extension
  work?
- **OQ7 — Work-queue fairness/ordering & thundering herd.** The queue is a query
  (§5.3); FIFO / priority / fairness across executors and the single-event
  stampede (§5.4) are executor policy. Is any specific ordering / backoff /
  work-stealing behaviour required?
- **OQ8 — Server-side monotonic fence + executor-clock `lease_until` (primitive
  extension).** Safe fencing needs the claim to increment the fence **server-side,
  atomically inside its commit-guarded critical section** (or enforce
  "supplied fence > stored fence, else abort") — today's `claim_with_lease` does
  neither (§5.3). Independently, `lease_until` is computed on the *executor's*
  clock (liveness hazard, §5.3); should the DB compute
  `lease_until = commit_HLC + TTL` or clamp it to `commit_HLC + max_ttl`? Both are
  the Phase 3e change. Confirm scope and shape.
- **OQ9 — Performance / write-amplification envelope.** One GroupCommit fsync per
  step, per-step memoize reads, and unbounded graph growth for long runs are
  unquantified (§5.7, N7). What throughput target and benchmark set should Phase
  3a prototyping establish (per CLAUDE.md's benchmark mandate)?
- **OQ10 — Namespace / RBAC scoping of journals.** The changefeed is
  principal-scoped and agent-scoped namespaces exist (#3349,
  `src/core/namespace.rs`). Are workflow journals **namespace-isolated** (per
  agent) or **global**? This directly affects the "queue is a query" claim (§5.3):
  a namespace-isolated queue query sees only its own runs, and cross-namespace
  stealing/recovery would need explicit scoping. Confirm the intended isolation
  model.

---

## 10. Acceptance-criteria / scope coverage

Issue #3577 is a *design sketch*, so coverage means "addressed by section," not
"code shipped."

| #3577 asked for | Addressed by |
|---|---|
| Frame the three DBOS store requirements (journal / claims / wakeups) | §1.1, §3 |
| Treat CAS/lease as a shipped building block, not new work | §3 (table + verified `claim_with_lease` contract + attribution note), §5.3 |
| Treat changefeed/notify (#3375) as shipped | §1.3, §3, §5.4 |
| Exactly-once step recording via `apply_batch` + unique constraints | §5.2 (`src/mcp/batch.rs`, `src/db/ops.rs:1433`, `src/db/schema_constraint.rs:298`) |
| Atomic claims for recovery / queues / timers | §5.3, §5.6 |
| Wakeups (LISTEN/NOTIFY) | §5.4 |
| Workflow-journal schema convention | §5.1 |
| Reference executor loop | §5.6 |
| Bi-temporal observability edge over Postgres (time-travel + audit) | §1.2, §5.5 |
| Where it should live (library / plugin / autumn-side) | §7, §9 OQ1 |
| Phasing (note the primitives are in-tree) | §7 |
| **Honest gaps** (fencing needs a primitive extension; no CAS-in-batch; executor-clock lease liveness; orchestration determinism; in-memory lineage; side-effect exactly-once; cold-tier retention; write-amplification; namespace scoping) | §2 (N2/N6/N7), §5.3, §5.7, §6.2, §8, §9 (OQ6/OQ8/OQ9/OQ10) |

---

## References

- Issue #3577 (this design; also the in-source stamp on the CAS/lease primitive),
  #3604 (referred to as CAS/lease in `src/core/namespace.rs:11` — see the §3
  attribution note), #3375 (changefeed, in-tree), #3574 / PR #3573 (lost-write
  hardening — a git-history/PR reference, no source-verifiable symbol in this
  worktree), #3374 (point-in-time restore, `src/db/pitr.rs`), #3351 / #3513 (hash
  chain), #3427 (principal provenance), #3371 / #3413 (lineage + durable
  follow-up), #3231 (`apply_batch`; a CAS-in-batch op would extend it — OQ6),
  #3218 / #3378 (constraints), #3234 (structured errors), #3349 (namespaces —
  OQ10).
- Source: `src/api/transaction/write/cas.rs`, `src/db/ops.rs`,
  `src/mcp/batch.rs`, `src/db/changefeed_sub.rs`,
  `src/core/changefeed_subscription.rs`, `src/db/temporal.rs`,
  `src/core/history.rs`, `src/core/lineage.rs`, `src/db/lineage.rs`,
  `src/core/namespace.rs`, `src/core/hlc.rs`,
  `src/storage/wal/durability.rs`, `src/db/schema_constraint.rs`,
  `src/provenance_chain/`, `src/db/snapshot.rs`, `src/db/backup.rs`,
  `src/db/pitr.rs`, `src/storage/checkpoint.rs`.
- Guides: [reacting-to-change](../guides/reacting-to-change.md),
  [derivation-lineage](../guides/derivation-lineage.md),
  [provenance-hash-chain](../guides/provenance-hash-chain.md),
  [schema-constraints](../guides/schema-constraints.md),
  [mcp-query-tool](../guides/mcp-query-tool.md),
  [snapshot-pin](../guides/snapshot-pin.md),
  [namespaces-guide](../guides/namespaces-guide.md),
  [WAL](../WAL.md), [backup-restore](../guides/backup-restore.md).
