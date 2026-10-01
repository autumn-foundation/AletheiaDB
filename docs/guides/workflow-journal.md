# Workflow Journal — Durable Execution (Phase 3a)

> **Status:** experimental, behind the `durable-execution` feature flag.
> This is **Phase 3a only** — the schema convention and the exactly-once
> step-recording building block. The executor loop, claims/leases, durable
> timers, wakeups, and safe fencing are **not** in this wave (see
> [Scope](#scope-what-is-and-is-not-in-3a)).

A [DBOS](https://www.dbos.dev/)-style durable-execution engine runs a
multi-step workflow as ordinary code that survives process crashes. It does so
by writing every significant effect — each **step** — into a durable **journal**
*before* returning its result. On restart the engine **replays** the workflow
function: steps whose result is already in the journal are not re-executed;
their recorded output is returned directly (**memoization**). That is what makes
execution "exactly-once" from the workflow's point of view even though the
process may have crashed and restarted arbitrarily many times.

The workflow journal builds that durable, exactly-once **recording** primitive
on top of AletheiaDB's existing bi-temporal store — no core storage, WAL, or
on-disk-format changes. It is a thin *schema convention* plus a small typed API.

## Enabling the feature

The module is compiled only when the `durable-execution` feature is on. It is a
standalone experimental flag (empty deps), **not** folded into the `nova`
umbrella — mirroring `semantic-retrieval-fusion` and the ADR-0050 graduation
pattern.

```toml
# Cargo.toml
[dependencies]
aletheiadb = { version = "0.2", features = ["durable-execution"] }
```

```rust
use aletheiadb::{
    AletheiaDB, CreateRunSpec, StepRecordSpec, StepExecError, WorkflowJournalExt,
};

let db = AletheiaDB::new()?;
let journal = db.workflow_journal();

// Enable the UNIQUE constraints the journal depends on (idempotent — safe to
// call on every process start).
journal.bootstrap()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`workflow_journal()` is an extension-trait method (`WorkflowJournalExt`) that
returns a lightweight, stateless `WorkflowJournal<'_>` handle borrowing the
database.

## Schema convention (§5.1)

The journal models a run and its steps with **two node tables** plus **one edge
type**. All property keys are exposed as typed `KEY_*` / `LABEL_*` / `EDGE_*`
constants on the module so callers never hand-write a magic string.

### `WorkflowRun` node (label `"WorkflowRun"`)

| Property | Type | Meaning |
|---|---|---|
| `workflow_id` | String | Caller-supplied stable id. **UNIQUE** `(WorkflowRun, workflow_id)`. |
| `name` | String | Workflow function name / type. |
| `status` | String | `pending` / `running` / `completed` / `failed`. |
| `owner` | String | Executor id holding the lease (absent if unclaimed). *Reserved for 3b.* |
| `lease_until` | Int (micros) | Lease deadline. *Reserved for 3b.* |
| `fence` | Int | Monotonic fencing token. *Reserved — safe fencing lands in 3e (see scope).* |
| `input` | Bytes | Opaque serialized workflow input. |
| `created_at` | Int (micros) | Creation time. |
| `wake_at` | Int (micros) | Durable-timer coordinate. *Reserved for 3c.* |

### `Step` node (label `"Step"`)

| Property | Type | Meaning |
|---|---|---|
| `workflow_id` | String | Owning run. |
| `step_number` | Int | Position in the run — the memo-key axis (replay order). |
| `idem_key` | String | `"{workflow_id}:{step_number}"`. **UNIQUE** `(Step, idem_key)` — the exactly-once guard. |
| `name` | String | Step function name — **also the replay-divergence guard** (see below). |
| `status` | String | `completed` / `failed`. |
| `output` | Bytes | Memoized result returned on replay. |
| `error` | String | Failure message when `status = failed`. |

### Edge

`WorkflowRun -[:HAS_STEP]-> Step` — the run→step index. Written atomically with
its `Step` node (see below).

`bootstrap()` enables the two UNIQUE constraints. It is **idempotent**: it reads
`list_unique_constraints()` first and skips any already declared, so calling it
on every startup never errors.

## Exactly-once step recording (§5.2)

`record_step` writes a step's outcome. The `idem_key` UNIQUE constraint is the
exactly-once anchor. The flow is:

```text
record Step{idem_key, name, status, output}  +  edge HAS_STEP run -> Step
        │  (both in ONE atomic write transaction, all-or-nothing)
        ▼
  ┌── commit succeeds ─────────────► fresh record written  (deduplicated = false)
  │
  └── UNIQUE(idem_key) violation ──► someone already recorded this step:
            re-read the existing Step by idem_key
            assert existing.name == spec.name           ── else FAIL LOUD:
                                                            OrchestrationDivergence
            adopt the winner: return its recorded output (deduplicated = true)
```

Two properties matter:

1. **Idempotent under retry / claim race.** Only one `Step` row can exist per
   `idem_key`. A retried (or concurrent duplicate) record hits the constraint,
   re-reads, and **adopts the winner** — it never writes a second row and never
   returns a *foreign* step's output.
2. **Fails loud on orchestration divergence.** Before adopting, we assert the
   recorded step's `name` equals the caller's expected name. A mismatch means
   two orchestrators disagree about what step N *is* (replay invoked a different
   operation at the same positional `step_number`). Because the memo keys on
   position, silently returning position N's stored output for a now-*different*
   logical operation would be a correctness bug, so we return
   `WorkflowError::OrchestrationDivergence` instead (fail-loud, N6 / §8 #12).
   The guard cannot *repair* nondeterminism; it only refuses to compound it.

#### The reserved-before-applied window (concurrent adopt-winner retry)

Under a *real* concurrent duplicate-record race there is a narrow window where
the `idem_key` UNIQUE **reservation** is already visible to the losing writer
**before** the winner's `Step` node has been applied to current storage — the
winner may be parked in a GroupCommit fsync wait at that instant. A naive
re-read in that window would transiently return `None` and spuriously fail the
§5.2 / §8 #1 idempotency contract.

`record_step` therefore makes the adopt-winner re-read a **bounded retry loop**:
it re-reads the winner up to a small fixed number of times with a short
exponential backoff (the reserved winner is guaranteed to become visible
shortly). If the winner *still* cannot be read after the bounded retries,
`record_step` returns the **retriable** `WorkflowError::Contended` — never a
terminal `Malformed` — so the caller simply retries the record. The
name-divergence guard still runs the instant a record is found, so a genuine
mismatch fails loud even under contention. The same `Contended` error is
surfaced when the underlying write aborts with a transient serialization /
write conflict; all other database errors pass through as `Database`.

### Why the library `db.write` transaction, not the MCP `apply_batch`

The design describes recording "in one `apply_batch`", but `apply_batch` is an
**MCP-surface** tool (`AletheiaMcpServer`, the `mcp-server` feature). A
library-level module must not depend on the MCP server. The equivalent
primitive — and the one `apply_batch` itself composes over — is the
write-transaction closure `AletheiaDB::write(|tx| …)`, which commits all of its
operations **all-or-nothing** in one `WriteTransaction` (a single WAL batch
append / group-commit fsync). `record_step` uses it directly:

```rust
let step_id = self.db.write(|tx| {
    let step_id = tx.create_node_with_valid_time(LABEL_STEP, step_props, valid_time)?;
    tx.create_edge_with_valid_time(run.node_id(), step_id, EDGE_HAS_STEP, props, valid_time)?;
    Ok(step_id)
});
```

So the `Step` node and its `HAS_STEP` edge are committed atomically. A
`UNIQUE(idem_key)` violation surfaces from that call as
`Error::Constraint(ConstraintError::UniqueViolation { .. })`, which is how the
exactly-once collision is detected and routed to the adopt-winner branch.

## Public API

```rust
use aletheiadb::{
    AletheiaDB, CreateRunSpec, StepRecordSpec, StepExecError, StepStatus,
    WorkflowJournalExt,
};

let db = AletheiaDB::new()?;
let journal = db.workflow_journal();
journal.bootstrap()?;                                   // idempotent

// Create a run (fails RunAlreadyExists on a duplicate workflow_id).
let run = journal.create_run(
    CreateRunSpec::new("wf-123", "checkout").input(b"payload".to_vec()),
)?;

// Record a completed step (idempotent, exactly-once).
let outcome = journal.record_step(
    &run,
    StepRecordSpec::completed(1, "charge_card", b"receipt-abc".to_vec()),
)?;
assert!(!outcome.deduplicated);

// Memoize-or-execute driver: run exec() only if step 1 is not already recorded.
let value = journal.get_or_record_step(&run, 1, "charge_card", || {
    // ... perform the real side-effecting work ...
    Ok::<Vec<u8>, StepExecError>(b"receipt-abc".to_vec())
})?;
assert!(value.from_memo);        // step 1 already existed → exec() not called

// Read back.
let step = journal.get_step("wf-123", 1)?;               // Option<StepRecord>
let all  = journal.list_steps("wf-123")?;                // sorted ASC by step_number
# Ok::<(), Box<dyn std::error::Error>>(())
```

Key methods on `WorkflowJournal<'_>`:

| Method | Purpose |
|---|---|
| `bootstrap()` | Idempotently enable the two UNIQUE constraints. |
| `create_run(spec)` | Create a `WorkflowRun`; `RunAlreadyExists` on duplicate id. |
| `get_run(workflow_id)` | Look up a run (`Option<WorkflowRun>`). |
| `record_step(run, spec)` | Exactly-once record → `StepOutcome { record, deduplicated }`. |
| `get_step(workflow_id, n)` | Fetch one step by `idem_key` (`Option<StepRecord>`). |
| `list_steps(workflow_id)` | All steps, **sorted ascending by `step_number`** (replay order, never `valid_from`). |
| `get_or_record_step(run, n, expected_name, exec)` | Memoize-or-execute driver (see below). |

### `get_or_record_step` — the memoize-or-execute driver

```text
if step N already recorded:
    assert name == expected_name          ── else OrchestrationDivergence (no exec)
    completed → return memoized output     (from_memo = true, exec NOT called)
    failed    → propagate StepFailed
else:
    out = exec()                           (the real, possibly non-idempotent work)
    record_step(run, completed(N, expected_name, out))   (resolves any record race)
    return out
```

On a memo hit the executor closure is **never invoked** — that is the whole
point of memoized replay. A completed step returns its stored output; a failed
step propagates its recorded error; a name mismatch fails loud without running
`exec`.

### Backdated valid time

`StepRecordSpec::completed(..).with_valid_time(ts)` records the step at a
specific (possibly backdated) bi-temporal `valid_from` coordinate, via the
`create_node_with_valid_time` / `create_edge_with_valid_time` variants. This
affects **only** the valid-time axis of the written facts; **replay order is
always by `step_number`**, so a backdated record never reorders steps
(§8 #8).

### Errors

`WorkflowError` is a **module-local** error type (deliberately *not* folded into
the crate-wide `aletheiadb::Error`). Underlying storage errors are carried
transparently via `WorkflowError::Database`. Notable variants:

| Variant | When | Retriable |
|---|---|---|
| `RunAlreadyExists` | `create_run` with a `workflow_id` that already exists. | no |
| `OrchestrationDivergence` | Recorded step name ≠ expected name (fail-loud, §8 #12). | no |
| `StepFailed` | A memo hit (in `get_or_record_step`, on both the direct memo-hit path and the adopt-after-race path) on a step recorded as `failed`; its recorded error is propagated (never an empty success). | no |
| `StepExecutionFailed` | The user `exec` closure returned an error (step **not** recorded → a later retry re-executes). | no |
| `Contended` | Transient contention: a concurrent duplicate won the `idem_key` reservation but its `Step` node was not yet visible after the bounded adopt-retry, or the write aborted with a transient serialization / write conflict. **Retry the `record_step`.** | **yes** |
| `Malformed` | A stored record is missing a required property / holds an unexpected type (external corruption). | no |
| `Database` | Any other error from the underlying store. | no |

## Scope: what is (and is NOT) in 3a

**In 3a (this wave):**

- The schema convention — both node tables (`WorkflowRun`, `Step`) and the
  `HAS_STEP` edge.
- Exactly-once step recording (`CONSTRAINT_VIOLATION` → re-read → adopt-winner,
  with the name-divergence fail-loud guard).
- The memoize-or-execute driver (`get_or_record_step`).
- Typed accessors, enums (`WorkflowStatus` / `StepStatus`), and builders
  (`CreateRunSpec` / `StepRecordSpec`).

**NOT in 3a (later phases):**

- **Executor loop** — the replay driver that runs a whole workflow function.
- **Claims / leases** — atomic single-owner claiming of a run
  (`claim_with_lease` exists in-tree but is **not** used here).
- **Durable timers & wakeups** — `wake_at` scheduling and changefeed-driven
  notification (3b/3c).
- **Safe multi-executor fencing (3b) is gated on the 3e fence primitive.** The
  schema reserves a `fence` slot on `WorkflowRun`, but **no fencing logic is
  implemented in this wave**. Sound fencing needs a monotonic
  compare-and-increment primitive AletheiaDB does not have today — on the
  current CAS+lease primitive two stealers can compute the same fence and a
  zombie writer is **not reliably** fenced out (design §5.3, §8 rows #4/#16).
  Do **not** rely on `fence` for correctness until 3e lands.

See the design sketch
[`docs/plans/2026-07-19-dbos-design-sketch.md`](../plans/2026-07-19-dbos-design-sketch.md)
(§5.1 schema, §5.2 recording, §8 acceptance table) for the full rationale.
