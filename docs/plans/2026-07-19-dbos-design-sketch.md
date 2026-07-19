# Design sketch: AletheiaDB as a DBOS-style durable-execution store (Issue #3577)

Status: **design-only draft** · Base: origin/trunk @ 65b67f2 · Date: 2026-07-19 · Author: implementation agent

> DBOS-style durable execution needs three things from its backing store: a
> transactional **step journal** with exactly-once recording, **atomic claims**
> (executor recovery, work queues, durable timers), and **wakeups**
> (LISTEN/NOTIFY-style notification). AletheiaDB is unexpectedly close to being
> that store — and its bi-temporality makes it *strictly more capable* than
> Postgres for the observability half. Two of the three primitives already
> shipped: the compare-and-swap + lease write primitive (Issue #3604, in
> `src/api/transaction/write/cas.rs`) and the changefeed / notify primitive
> (Issue #3375, `subscribe_changes` / `list_changes`). This document designs the
> remaining **Phase 3**: a workflow-journal *schema convention* and a *reference
> executor loop* composed on those existing primitives. It is **design-only** —
> no source changes are proposed to land in this PR.

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
   claimant).
3. **Wakeups.** Executors must not busy-poll. They need a LISTEN/NOTIFY-style
   signal — "a new run is claimable" / "this run's status changed" — to block
   until there is work.

### 1.2 Why AletheiaDB as the store

- **Durability core already present.** Acked-write-survives-crash is the WAL +
  group-commit contract (`src/storage/wal/durability.rs`); the invariant was
  hardened by the #3574 lost-write fix (PR #3573). Point-in-time restore
  (backup/restore, `src/db/backup.rs`) covers disaster recovery.
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

Phase 1 (CAS/lease) shipped via **#3604**. Phase 2 (changefeed/notify) landed via
**#3375**. This doc does **not** re-propose them; it treats them as available
building blocks (§4) and designs **Phase 3** on top: the journal schema
convention (§6.1–6.2), the claim/recovery/queue/timer mapping onto CAS+lease
(§6.3), the wakeup mapping onto the changefeed (§6.4), the time-travel /
audit story (§6.5), and a reference executor loop (§6.6).

---

## 2. Goals / Non-Goals

### Goals

- **G1.** Define a **workflow-journal schema convention** (node/edge shapes,
  property keys, constraints) expressible on today's AletheiaDB primitives.
- **G2.** Show **exactly-once step recording** composed from `apply_batch`
  (atomic multi-op) + a uniqueness constraint on the idempotency key.
- **G3.** Map **atomic claim / recovery / work-queue / durable-timer** semantics
  onto the shipped CAS + lease primitive (`claim_with_lease`), including the
  **fencing-token** stale-write guard.
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
  perform → record-result" discipline (§9, risk #6).
- **N3.** **No distributed executor consensus beyond leases.** Ownership is
  first-committer-wins CAS + time-based lease expiry, not Raft/Paxos. Correctness
  under clock skew is bounded by HLC drift handling (`src/core/hlc.rs`), not a
  consensus protocol.
- **N4.** **No language SDKs / bindings** designed here.
- **N5.** **Not re-designing #3604 or #3375.** Both shipped; they are inputs, not
  deliverables. Any change to their public surface is out of scope.

---

## 3. Background: existing primitives we build on

Every claim below is cited to source in this worktree. Where a primitive only
*partially* fits the DBOS need, that is called out honestly.

| DBOS requirement | AletheiaDB primitive | Source (verified) | Fit |
|---|---|---|---|
| Durable, acked-write-survives-crash journal | WAL durability modes `Synchronous` / `GroupCommit` / `Async` / `AsyncBatched` | `src/storage/wal/durability.rs:52,66,82,92` | **Full.** `GroupCommit` (ACID, batched fsync) is the default journal mode; `Synchronous` for zero-loss; `Async` is *not* ACID (§10 open question on default). |
| Exactly-once step recording (atomic multi-op) | `apply_batch` — ordered ops, all-or-nothing, single `WriteTransaction` / one GroupCommit fsync | `src/mcp/batch.rs:1,415` | **Full** for the recording atom. |
| "record iff idempotency key unused" | uniqueness constraint (`unique_constraint`) + schema constraints (#3378) | `src/db/ops.rs:1433`, `src/db/schema_constraint.rs:298` | **Full.** A duplicate `(workflow_id, step_number)` write fails at the pre-apply commit hook → `CONSTRAINT_VIOLATION`. |
| Atomic claim / lease / fencing | `claim_with_lease` / `claim_with_lease_with_options`; `compare_and_set_node` / `compare_and_set_edge` (+`_with_options`) | `src/db/ops.rs:728,762,666,695,707`; contract in `src/api/transaction/write/cas.rs` | **Full.** Conditional **full-replace**; lease branch succeeds iff version matches OR `lease_until` (int micros) `<=` commit HLC. "Exactly one winner" under the commit-serialization guard. |
| Wakeups (LISTEN/NOTIFY) | `subscribe_changes(ChangeFilter) -> Subscription`; `Subscription::poll()` / `recv_timeout(dur)` | `src/db/changefeed_sub.rs:55`; `src/core/changefeed_subscription.rs:396,417,433` | **Full** for push. Best-effort at-least-once; durable ground truth is pull `list_changes`. |
| Durable, replayable changefeed (lag recovery) | `list_changes(&ChangeFeedQuery) -> ChangeFeedPage`, `resume_token` / `ChangeCursor` dedup | `src/db/temporal.rs:562` | **Full.** Rebuilt by WAL recovery; lagged consumer resumes losslessly. |
| Time-travel debugging of past runs | Bi-temporal `VersionInfo`; `AS OF` valid/tx time reads | `src/core/history.rs:45` | **Full** — the differentiator vs Postgres. Caveat: cold-tier eviction / truncation can render a very old pinned version unreadable (§9 risk #10). |
| Fact-to-fact derivation (step → derived KG facts) | `LineageRef`, `create_node_with_lineage`, upstream/downstream closure | `src/core/lineage.rs:53`, `src/db/lineage.rs:120` | **Partial.** v1 lineage index is **in-memory only** — it does *not* survive restart (`src/core/lineage.rs:34`). Journal correctness must **not** depend on it (§9 risk #7). |
| Tamper-evident audit | Provenance hash chain (#3351), principal provenance (#3427) | `src/provenance_chain/` (engine/record/store/verify) | **Full** for auditability; principal stamping covers structured create/update paths (deletes/retracts not yet — #3427). |
| Reproducible run views | Named snapshots (`create_snapshot`) | `src/db/snapshot.rs` | **Full** for pinning a run's KG view. |
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
`TransactionError::CasMismatch` and writes nothing.

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
  constraints); traversal cost for very long runs; requires the executor to
  follow the schema convention faithfully.

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
the run/step *nodes* and the exactly-once recording (§6.1–6.2) are identical in
both; A simply *also* materializes the `HAS_STEP` / `DERIVED` edges. An
implementation can ship B first (nodes + constraints + claim/notify) and add the
edges/lineage in a later phase (§8, Phase 3d) without a data migration — the
edges are additive over the same nodes.

---

## 5. (folded into §4 above — approaches, pros/cons, decision)

*(Section intentionally merged with §4 to keep the approach analysis in one
place; numbering preserved so cross-references to "§6 Design" stay stable.)*

---

## 6. Design (chosen: graph-native journal)

All shapes below are a **convention over existing primitives** — no new storage
format, no new WAL frame, no core API is proposed here.

### 6.1 Workflow-journal schema convention

**`WorkflowRun` node** (label `WorkflowRun`):

| Property | Type | Meaning |
|---|---|---|
| `workflow_id` | String | Caller-supplied stable id; **unique constraint** on `(WorkflowRun, workflow_id)`. |
| `name` | String | Workflow function name / type. |
| `status` | String | `pending` / `running` / `completed` / `failed`. |
| `owner` | String | Current executor id holding the lease (empty/absent if unclaimed). |
| `lease_until` | Int (micros) | Lease deadline; the `lease_until_key` fed to `claim_with_lease`. |
| `fence` | Int | Monotonic **fencing token**, bumped on every successful claim. |
| `input` | (blob) | Workflow input payload (see §9 risk #9 on size). |
| `created_at` | Int (micros) | Enqueue time (also the `valid_from` if backdated). |
| `wake_at` | Int (micros) | Durable-timer coordinate (absent unless timer-scheduled). |

**`Step` node** (label `Step`):

| Property | Type | Meaning |
|---|---|---|
| `workflow_id` | String | Owning run. |
| `step_number` | Int | Position in the run. |
| `idem_key` | String | `"{workflow_id}:{step_number}"`; **unique constraint** on `(Step, idem_key)` — the exactly-once guard. |
| `name` | String | Step function name. |
| `status` | String | `completed` / `failed`. |
| `output` | (blob) | Memoized result returned on replay. |
| `error` | String | Error payload when `status = failed`. |

**Edges:** `WorkflowRun -[:HAS_STEP]-> Step` (run→step index for traversal);
optionally `Step -[:DERIVED]-> <fact node>` recorded via `create_edge_with_lineage`
/ `derived_from` (`src/db/lineage.rs:120`) so a step's knowledge-graph outputs are
version-pinned to exactly the fact versions it wrote.

**Constraints** (both enabled once at bootstrap, `src/db/ops.rs:1433` /
`src/db/schema_constraint.rs:298`): unique `(WorkflowRun, workflow_id)` and unique
`(Step, idem_key)`. These are enforced at the pre-apply commit hook for **all**
write paths including `apply_batch`, so one violating op aborts the whole
transaction with zero partial writes.

### 6.2 Exactly-once step recording

Each step result is recorded in **one** `apply_batch` transaction
(`src/mcp/batch.rs:415`) containing: `create_node` for the `Step` (carrying
`idem_key`) and `create_edge` for `HAS_STEP` (referencing the step via a batch
`$alias`), all-or-nothing. Because `(Step, idem_key)` is unique, a **retried**
re-record of the same step hits `CONSTRAINT_VIOLATION` (#3234) and writes nothing.

**Read-memoized-on-replay flow.** On (re)entering step N the executor first reads
the `Step` node for `idem_key = "{workflow_id}:{N}"`:

```
result = find Step where idem_key = "{W}:{N}"
if result exists:            # journal already has it (memoized)
    if result.status == completed: return result.output   # skip re-execution
    else: propagate result.error
else:                        # first execution
    out = execute_step_N()   # the real (possibly non-idempotent) work
    try apply_batch([ create Step{idem_key, status:completed, output:out},
                      create edge HAS_STEP run->$step ])
    on CONSTRAINT_VIOLATION:  # a concurrent/duplicate executor won the race
        result = re-read Step where idem_key = "{W}:{N}"
        return result.output   # adopt the winner's recorded result
    return out
```

The `CONSTRAINT_VIOLATION`→re-read branch makes recording **idempotent even under
a claim race** (two executors briefly both believing they own the run): only one
`Step` row can exist per `idem_key`; the loser adopts it. This is *recording*
exactly-once, not *side-effect* exactly-once (N2).

### 6.3 Atomic claim / recovery / queues / timers (CAS + lease)

**Claim.** An executor claims a `WorkflowRun` node by
`claim_with_lease(run_node, expected_version, "owner", "lease_until", owner_id,
now + lease_ttl, new_props)` (`src/db/ops.rs:728`). The full-replace `new_props`
set `status = running`, `owner = owner_id`, and **`fence = old_fence + 1`**. The
call wins iff the version matches or the stored `lease_until <= commit HLC` — so a
`pending` run (never claimed) *or* a run whose lease expired is claimable, and a
`running` run with a live lease is not.

**Crash recovery.** No special path: a crashed executor simply stops renewing.
When its `lease_until` passes, *any* executor's `claim_with_lease` finds the lease
expired and steals the run (bumping `fence`). First-committer-wins under the
commit guard guarantees exactly one successor.

**Work queue.** The queue is a *query*, not a structure: the set of claimable runs
= `find_nodes` where `label = WorkflowRun AND status = pending` **plus** running
runs whose `lease_until < now` (expired). An executor pulls a candidate and
attempts to claim; losers move to the next candidate. (Fairness/ordering is
executor policy — §10 open question.)

**Durable timer.** A scheduled wake is a `WorkflowRun` (or a dedicated `Timer`
node) with `status = pending` and `wake_at = T`. It is *not claimable* until
`now >= wake_at` (executors filter the queue query by `wake_at <= now`). Firing =
claiming it via the same lease path. Durability is free: `wake_at` is a persisted
property surviving crash/restart.

**Fencing (stale-write guard).** The `fence` token defends against a *zombie*
executor — one that was paused (GC, VM stall) past its lease, had the run stolen,
then wakes and tries to write. Every journal write the executor makes carries the
`fence` value it claimed with; a step-record `apply_batch` includes a
`compare_and_set_node` (`src/db/ops.rs:666`) on the run asserting `fence` is still
the claimed value. If a newer executor bumped `fence`, the zombie's CAS fails
(`CasMismatch`) and its whole batch aborts — it cannot corrupt the journal after
losing ownership:

```
# zombie holds fence=7, but run was re-claimed → fence=8
apply_batch([ CAS run WHERE fence==7 -> (no-op touch),   # FAILS: fence is 8
              create Step{...} ])                          # whole batch aborts
```

*(This composition of "CAS the run's fence inside the step-recording batch" is the
design; whether `apply_batch` can host a CAS/lease op alongside creates is an
implementation detail to confirm — see §10 open question OQ6.)*

### 6.4 Wakeups (changefeed #3375)

Executors block instead of polling. Each subscribes:
`subscribe_changes(ChangeFilter::all().with_node_labels(["WorkflowRun"]).with_change_types([Created, Updated]))`
(`src/db/changefeed_sub.rs:55`; filter builders verified at
`src/core/changefeed_subscription.rs:189+` — fields `node_labels`, `edge_types`,
`change_types`, `namespace`; unset dimension = match-all). The returned
`Subscription` exposes `poll()` (non-blocking drain) and `recv_timeout(dur)`
(Mutex+Condvar long-poll — the LISTEN/NOTIFY wakeup;
`src/core/changefeed_subscription.rs:433`). A new `pending` run (Created) or a
status change (Updated) wakes a waiting executor, which then runs the queue query
(§6.3) and attempts a claim.

**Lag / overflow recovery.** The push feed is **best-effort at-least-once**. A
lagged consumer (bounded buffer overflow → disconnect) resumes **losslessly** by
pulling `list_changes` (`src/db/temporal.rs:562`) from its last `resume_token`
(a `ChangeCursor`), deduping by the stable `(tx_time, kind, entity_id,
version_id)` key. So the wakeup is an *optimization* over polling; correctness
never depends on a notification arriving — the durable `list_changes` pull is the
ground truth. (MCP surface: `await_changes` long-poll, deliberately excluded from
the #3368/#3353/#3360 wrappers since a long-poll is expected to block.)

### 6.5 Time-travel debugging & tamper-evident audit

- **Time-travel.** Because every `WorkflowRun`/`Step` write is a bi-temporal
  version (`src/core/history.rs:45`), `AS OF SYSTEM_TIME <t>` reconstructs the
  **exact journal** as it stood at transaction-time `t`: replay or inspect any
  historical run at any past instant, see a step's memoized output as first
  recorded even after later corrections, and answer *"what did the agent know when
  it took step N"* by reading the shared KG at that same coordinate. This is the
  concrete Postgres-beating differentiator (§1.2). Caveat: reconstructing a very
  old run requires anchoring **both** valid and transaction time before any
  superseding write, and cold-tier eviction/truncation bounds how far back a
  pinned version stays readable (same caveat `temporal_extent` documents).
- **Audit / tamper-evidence.** Principal provenance (#3427) stamps the executor
  principal into version provenance on structured create/update paths; the
  provenance **hash chain** (`src/provenance_chain/`, #3351) makes the journal
  tamper-evident — `aletheia verify` / the `verify_chain` MCP tool detect any
  retroactive edit. Honest gap: deletes/retracts do not yet stamp a principal
  (#3427), so a journal that only ever *appends* step rows (recommended — never
  delete a step) stays fully attributed.

### 6.6 Reference executor loop (pseudocode)

```
loop:
    # 1. Wait for work without polling
    subscription.recv_timeout(idle_backoff)         # §6.4 wakeup

    # 2. Find & claim a run (queue = a query, §6.3)
    for run in find WorkflowRun where
              status == 'pending' OR lease_until < now(),
              and (wake_at is absent OR wake_at <= now()):   # durable timers
        try:
            v = claim_with_lease(run, run.version,
                                 owner=me, lease_until=now()+LEASE_TTL,
                                 props={status:'running', owner:me,
                                        fence:run.fence+1})   # §6.3 claim+fence
            my_fence = run.fence + 1
        except CasMismatch:
            continue          # someone else won; try next candidate
        break
    else:
        continue              # nothing claimable; loop back to wait

    # 3. Execute steps with journal memoization (§6.2)
    spawn lease_renewer(run, me):        # periodically re-claim to extend lease
        every LEASE_TTL/3:
            claim_with_lease(run, ..., lease_until=now()+LEASE_TTL,
                             props={..., fence:my_fence})   # keep same fence

    for step_number in workflow.steps:
        memo = find Step where idem_key == "{run.workflow_id}:{step_number}"
        if memo exists:
            result = memo.output                      # skip re-execution (replay)
        else:
            out = execute(step_number)                # real work (§9 risk #6)
            try:
                apply_batch([
                    CAS run WHERE fence == my_fence,   # fencing guard (§6.3)
                    create Step{idem_key, status:'completed', output:out},
                    create edge (run)-[:HAS_STEP]->($step),
                ])
            except CasMismatch:                        # lease lost mid-step
                abort_run_processing()                 # a successor owns it now
            except ConstraintViolation:                # duplicate record
                result = re-read Step{idem_key}.output # adopt winner (§6.2)
            else:
                result = out

    # 4. Mark run done (final claim-fenced write)
    apply_batch([ CAS run WHERE fence == my_fence,
                  update run {status:'completed', owner:'', lease_until:0} ])
    stop lease_renewer

# On crash anywhere above: the lease simply stops renewing; when lease_until
# passes, another executor claims (step 2) and resumes from the journal (step 3)
# — completed steps are memoized, so no step re-executes.
```

---

## 7. Brainstorm / Reverse-brainstorm / Six hats

### 7.1 Brainstorm (green hat) — capabilities this unlocks

- Native **time-travel replay** of any past run (`AS OF`) — reproduce a
  production incident by reading the journal as it was at the failing instant.
- **One substrate** for agent knowledge + agent workflow — "what did I know at
  step N" is a single bi-temporal read, no cross-store join.
- **Tamper-evident, attributed** step history (hash chain + principal) — audit
  without an external audit log.
- **Derivation lineage** from a step to the exact KG fact versions it produced
  (`Step -[:DERIVED]-> fact`, version-pinned) — blast-radius analysis of a bad
  step.
- **Durable timers for free** — a `wake_at` property, no separate scheduler store.
- **Backup/restore + named snapshots** give reproducible whole-engine
  point-in-time restore and pinned run views with no new machinery.

### 7.2 Reverse-brainstorm — how could this design fail / corrupt / mislead?

| Hazard | Consequence | Mitigation |
|---|---|---|
| **Non-deterministic replay** (a step reads wallclock / RNG / external state that changed) | Replay produces a different value than the memoized one → divergent workflow | Memoize **outputs**, never re-derive on replay; the journal's recorded `output` is authoritative. Document that steps must be pure-of-record: side effects go *inside* a step, results are journaled (N2). |
| **Lease race / zombie writer** | Two executors both write the journal → corruption | Fencing token: every journal write CASes the run's `fence` (§6.3); a stolen run bumps `fence`, the zombie's CAS aborts (`CasMismatch`). |
| **Notify loss / lag** | Executor sleeps through claimable work | Wakeup is best-effort; the durable `list_changes` pull + `resume_token` is ground truth; executors also wake on `recv_timeout` backoff and re-run the queue query. |
| **In-memory lineage lost on restart** (`src/core/lineage.rs:34`) | `DERIVED` closure empty after crash → misleads a blast-radius query | Journal **correctness must not depend on lineage**; `HAS_STEP` edges (durable graph edges, not the lineage index) carry run→step structure. Lineage is observability-only until #3413 makes it durable (§9 risk #7). |
| **Unbounded run retention vs cold-tier eviction** | Old runs migrate to cold / get truncated → `AS OF` replay of ancient runs fails | Document the retention/eviction interaction (§10 OQ3); pin critical runs with named snapshots; treat "run older than cold horizon" as expected-unreadable, not a bug. |
| **Step payload too large vs token/response budget** | Big `output`/`input` blobs bloat reads, blow MCP token budgets | Store large payloads by reference (hash → blob store) or accept vector/large-value elision (#3220/#3353); constrain step output size (§9 risk #9). |
| **Backdated `valid_time` reorders the journal** | A step recorded with a past `valid_from` looks out-of-order vs `step_number` | Journal **ordering is `step_number` (a property), never valid-time**; `valid_time` is metadata only. Recording backdating is allowed but does not reorder replay (§9 risk #8). |
| **`Async` WAL mode used for the journal** | Acked step "recorded" then lost on crash (Async is not ACID) | Default the journal to `GroupCommit`/`Synchronous`; forbid `Async` for the journal store (§10 OQ2). |

### 7.3 Six hats (condensed)

- **White (facts):** CAS/lease (#3604) and changefeed (#3375) are shipped and
  cited; `apply_batch` + unique constraints give the recording atom; bi-temporal
  versions give time-travel. Lineage index is in-memory (verified).
- **Red (gut):** the graph-native journal *feels* right — it is the one design
  that uses what makes AletheiaDB different; a KV-only journal feels like using a
  graph DB as a worse Postgres.
- **Black (risks):** replay determinism, zombie writers, notify loss, cold-tier
  eviction of old runs, payload size, in-memory lineage — each has a named
  mitigation (§7.2) and a test (§9).
- **Yellow (upside):** native time-travel debugging + tamper-evident audit +
  single-substrate agent state is a real, hard-to-replicate advantage over
  Postgres-backed DBOS.
- **Green (alternatives):** ship Approach B (KV journal) as v0, add graph
  edges/lineage (Approach A) additively; the engine could live as a library, an
  autumn plugin, or in-core (§10 OQ1).
- **Blue (process / next step):** this is a design sketch; next is coordinator
  review of the open questions (§10), then Phase 3a (schema + idempotent
  recording) behind the shipped primitives.

---

## 8. Not-in-scope recap & phased implementation outline

**Not in scope** (see §2 Non-Goals): full engine runtime, exactly-once external
side effects, executor consensus beyond leases, language SDKs, and any change to
#3604/#3375.

**Already DONE:** Phase 1 (CAS/lease, #3604) and Phase 2 (changefeed/notify,
#3375). The remaining work is Phase 3, sub-phased:

| Phase | Deliverable | Rough size |
|---|---|---|
| **3a** | Journal schema convention + idempotent step recording: `WorkflowRun`/`Step` labels, the two unique constraints, the `apply_batch` record + `CONSTRAINT_VIOLATION`→re-read flow (§6.1–6.2). Equivalent to Approach B v0. | **M** |
| **3b** | Reference executor loop: claim → memoized-step loop → lease renewer → completion; fencing composition (§6.6). Library/plugin, not core. | **M** |
| **3c** | Durable timers + work-queue query helpers + wakeup wiring (`subscribe_changes`/`recv_timeout`, lag recovery via `list_changes`) (§6.3–6.4). | **S–M** |
| **3d** | Graph-native observability (Approach A full): `HAS_STEP` / `DERIVED` edges + lineage, and a **time-travel run inspector** (`AS OF` journal reconstruction, hash-chain verify) (§6.5). | **L** |

Phases 3a–3c are shippable on today's primitives; 3d layers observability
additively over the same nodes (no migration).

---

## 9. Risks / edge-cases as test cases

| # | Scenario | Expected behavior | Test to write |
|---|---|---|---|
| 1 | **Duplicate step record under retry** | Second `apply_batch` for same `idem_key` → `CONSTRAINT_VIOLATION`; executor re-reads and returns the memoized output; exactly one `Step` node exists | `dup_step_record_is_idempotent` |
| 2 | **Executor crash mid-step** (work done, not yet recorded) | On recovery the step is *not* in the journal → successor re-executes it once; no double *record* | `crash_before_record_reexecutes_once` |
| 3 | **Two executors race to claim** one `pending` run | Exactly one `claim_with_lease` wins (`fence` bumped once); loser gets `CasMismatch`, tries next candidate | `concurrent_claim_single_winner` |
| 4 | **Lease expiry during a long step** (zombie) | Successor steals the run (bumps `fence`); zombie's fenced `apply_batch` aborts on `CasMismatch`; no journal corruption | `zombie_write_fenced_out` |
| 5 | **Notify overflow / lag** | Subscription disconnects; executor resumes from `list_changes` at last `resume_token`, dedups by `ChangeCursor`; no missed run | `notify_lag_recovers_via_list_changes` |
| 6 | **Non-deterministic step on replay** | Replay returns the **memoized** output, does not re-run the step; value is stable across replays | `replay_returns_memoized_not_recomputed` |
| 7 | **Restart loses in-memory lineage** (`src/core/lineage.rs:34`) | `HAS_STEP` graph edges + journal reads still fully reconstruct the run; only the `DERIVED` lineage closure is empty (observability-only, not correctness) | `journal_intact_after_lineage_reset` |
| 8 | **Backdated `valid_time` vs journal ordering** | Replay order follows `step_number`, not `valid_from`; a backdated record does not reorder steps | `backdated_valid_time_preserves_step_order` |
| 9 | **Step payload too large** | Oversize `output` handled by reference/elision; read stays within response/token budget; no unbounded blob in a read | `large_step_payload_bounded` |
| 10 | **`AS OF` replay of a cold-evicted old run** | Reconstruction of a run past the cold horizon fails cleanly (documented), not silently wrong | `as_of_beyond_cold_horizon_errors_cleanly` |
| 11 | **`Async` WAL mode misconfigured for journal** | Config guard rejects/ warns; journal defaults to `GroupCommit`/`Synchronous` | `journal_rejects_non_acid_wal` |

---

## 10. Open questions for the user

- **OQ1 — Where does the workflow engine live?** A standalone Rust library, an
  autumn-side plugin (matching the `aletheia serve` daemon plan), or in-core? The
  schema convention is store-agnostic; the executor loop is not.
- **OQ2 — Default WAL durability mode for the journal?** `GroupCommit` (ACID,
  high throughput) is the natural default; `Synchronous` for zero-loss at lower
  throughput. `Async`/`AsyncBatched` are *not* ACID
  (`src/storage/wal/durability.rs`) and must be forbidden for the journal —
  confirm.
- **OQ3 — Run retention vs cold-tier eviction / snapshot retention.** Time-travel
  replay of old runs conflicts with cold-tier migration/truncation. What is the
  retention policy, and should completed runs be pinned (named snapshots) or
  archived (backup) before they age out?
- **OQ4 — Exactly-once external side-effects: in or out of scope?** This design
  guarantees exactly-once *recording* only (N2). Is a side-effect idempotency-key
  convention (record-intent → perform → record-result) wanted in Phase 3, or left
  to callers?
- **OQ5 — Multi-executor fencing correctness proof.** Do you want a written
  argument (or model-checked spec) that fencing + first-committer-wins CAS admits
  no two-writer window under HLC clock skew, beyond the per-scenario tests (§9)?
- **OQ6 — CAS-op inside `apply_batch` (UNVERIFIED).** The fencing design (§6.3,
  §6.6) assumes a `compare_and_set`/lease op can be composed *inside* an
  `apply_batch` alongside `create` ops. I verified `apply_batch`
  (`src/mcp/batch.rs`) and `claim_with_lease` / `compare_and_set_node`
  (`src/db/ops.rs`) exist **independently**, but did **not** confirm that
  `apply_batch`'s op set includes a CAS/lease op. If it does not, the fencing
  guard must be a separate CAS transaction ordered before the record (a slightly
  weaker atomicity — a crash between the fence-CAS and the record leaves the step
  unrecorded, which is safe: it re-executes). Please confirm the intended
  composition.
- **OQ7 — Work-queue fairness/ordering.** The queue is a query (§6.3); FIFO /
  priority / fairness across executors is executor policy — is any specific
  ordering required?

---

## 11. Acceptance-criteria / scope coverage

Issue #3577 is a *design sketch*, so coverage means "addressed by section," not
"code shipped."

| #3577 asked for | Addressed by |
|---|---|
| Frame the three DBOS store requirements (journal / claims / wakeups) | §1.1, §3 |
| Treat CAS/lease (#3604) as a shipped building block, not new work | §3 (table + verified `claim_with_lease` contract), §6.3 |
| Treat changefeed/notify (#3375) as shipped | §3, §6.4 |
| Exactly-once step recording via `apply_batch` + unique constraints | §6.2 (`src/mcp/batch.rs`, `src/db/ops.rs:1433`, `src/db/schema_constraint.rs:298`) |
| Atomic claims for recovery / queues / timers | §6.3, §6.6 |
| Wakeups (LISTEN/NOTIFY) | §6.4 |
| Workflow-journal schema convention | §6.1 |
| Reference executor loop | §6.6 |
| Bi-temporal observability edge over Postgres (time-travel + audit) | §1.2, §6.5 |
| Where it should live (library / plugin / autumn-side) | §8, §10 OQ1 |
| Phasing (note #3604 done, #3375 done) | §8 |
| Honest gaps (in-memory lineage, side-effect exactly-once, cold-tier retention) | §2 (N2), §7.2, §9, §10 |

---

## References

- Issue #3577 (this design), #3604 (CAS/lease, shipped), #3375 (changefeed,
  shipped), #3574 / PR #3573 (lost-write fix), #3351 / #3513 (hash chain),
  #3427 (principal provenance), #3371 / #3413 (lineage + durable follow-up),
  #3231 (`apply_batch`), #3218 / #3378 (constraints), #3234 (structured errors).
- Source: `src/api/transaction/write/cas.rs`, `src/db/ops.rs`,
  `src/mcp/batch.rs`, `src/db/changefeed_sub.rs`,
  `src/core/changefeed_subscription.rs`, `src/db/temporal.rs`,
  `src/core/history.rs`, `src/core/lineage.rs`, `src/db/lineage.rs`,
  `src/storage/wal/durability.rs`, `src/db/schema_constraint.rs`,
  `src/provenance_chain/`, `src/db/snapshot.rs`, `src/storage/checkpoint.rs`.
- Guides: [reacting-to-change](../guides/reacting-to-change.md),
  [derivation-lineage](../guides/derivation-lineage.md),
  [provenance-hash-chain](../guides/provenance-hash-chain.md),
  [schema-constraints](../guides/schema-constraints.md),
  [mcp-query-tool](../guides/mcp-query-tool.md),
  [snapshot-pin](../guides/snapshot-pin.md),
  [WAL](../WAL.md), [backup-restore](../guides/backup-restore.md).
