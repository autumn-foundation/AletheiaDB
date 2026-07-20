# DBOS Phase 3e — Safe multi-executor fencing primitive extension

Status: **implementation** · Base: origin/trunk @ 7bcbd26 · Date: 2026-07-20
Implements the **OQ6 / OQ8** recommendations of
[`2026-07-19-dbos-design-sketch.md`](2026-07-19-dbos-design-sketch.md) §5.3 / §7 (Phase 3e).

> The design sketch's central negative finding is that the *recording* and
> *notify* halves of a DBOS-style store compose on shipped AletheiaDB
> primitives, but **safe multi-executor fencing does not** — it needs a small
> primitive extension (§5.3, OQ6/OQ8). This document designs and lands that
> extension in three pieces:
>
> 1. a **server-side monotonic fence precondition** on the claim (Piece 1, OQ8),
> 2. **DB-side lease-deadline computation** so `lease_until` no longer rides the
>    executor's clock (Piece 2, OQ8), and
> 3. a **compare-and-set op inside `apply_batch`** so a step-record batch can be
>    fenced atomically (Piece 3, OQ6).
>
> **The user may veto any of the three chosen shapes at review** — each shape is
> justified below with the alternatives considered.

---

## 0. Hard architectural constraint that drives every shape

The commit pipeline is, in order (`commit_with_timestamp_inner` →
`apply_changes`):

1. take the commit HLC under `current_timestamp`,
2. **serialize the buffered writes (their full property maps) to the WAL and
   fsync**,
3. acquire `historical.write()` (the commit-serialization guard) and run the
   authoritative CAS/write-skew re-checks (`detect_cas_precondition_violations`),
4. apply the buffered writes to in-memory current + historical state.

The load-bearing consequence: **a buffered write's property map is already
durably in the WAL before the commit-guard re-check runs.** So the commit-guard
phase can *reject* a transaction, but it **cannot mutate** a write's property
map (the mutated bytes would never reach the WAL, so a crash-replay would
diverge). Every shape below is chosen to be a **check that rejects**, never a
**mutation at apply time** — because the latter is unsound against WAL replay.

The commit-guard re-check (`detect_cas_precondition_violations`, run under
`historical.write()`) is exactly the serialization point where "two claimants
opened on the same snapshot cannot both pass": the second observes the first's
committed effect and aborts. Piece 1 rides this existing seam.

---

## 1. Piece 1 — server-side monotonic fence

### Problem (design §5.3, risk #16)

A `fence` token defends against a *zombie* executor (paused past its lease,
run stolen, then wakes and writes). The natural convention — read `old_fence`
in the queue query, stamp `old_fence + 1` — is **unsound today** because
(a) `claim_with_lease` performs no server-side fence bookkeeping (the fence is
just another value in the caller's full-replace map), and (b) the lease-steal
branch wins via `lease_until <= commit_HLC` *without* a version match, so two
stealers that both read `old_fence = 7` in their (earlier, un-serialized)
queue queries both stamp `fence = 8` — the **same** fence — defeating fencing
in exactly the recovery scenario it exists for.

### Approaches considered

- **(A) Server-side atomic increment.** The claim does not carry a fence; the
  DB reads the stored fence under the commit guard and stamps `stored + 1`.
  Collision-free *by construction* (each winning claim gets a distinct,
  strictly-increasing value) and impossible for a caller to misuse.
  **Rejected:** the incremented value must live in the committed property map,
  which is serialized to the WAL *before* the commit-guard re-check runs (§0).
  Implementing (A) would require either mutating a WAL-serialized map at apply
  time (unsound against replay) or a **separate server-maintained fence-counter
  store** with its own WAL framing — a far larger primitive than Phase 3e.

- **(B) Fence precondition ("supplied fence strictly greater than stored fence,
  else abort").** The caller stamps `new_fence` into its full-replace map (as it
  does today for any property); the DB, under the commit guard, re-reads the
  *committed* stored fence and rejects the claim unless `new_fence > stored`.
  **Chosen.** It fits the existing check-only `detect_cas_precondition_violations`
  architecture exactly — a purely additive re-read + comparison under the guard,
  no WAL/apply-path surgery, no new storage. It is precisely the shape the design
  sketch lists as the acceptable alternative in §5.3 ("or (b) `claim_with_lease`
  must gain a precondition …").

- **(C) Convention-only (status quo).** Already shown unsound (the hole).

### Chosen shape (B) and why the collision becomes IMPOSSIBLE

`FenceCondition { fence_key, new_fence }` is attached to the claim's
`CasPrecondition`. In `detect_cas_precondition_violations`, after the existing
`version-match OR lease-expired` gate passes, we additionally require
`new_fence > stored_fence`, where `stored_fence` is read from the **committed**
current-state node property `fence_key` (absent / non-integer ⇒ treated as
"no fence held", `i64::MIN`, so a first claim always passes). The whole claim
now succeeds iff `(version matches OR lease expired) AND new_fence > stored`.

Collision-impossibility argument (the guard serializes commits, so claims apply
one at a time and each re-reads committed state):

> Two claims both compute `new_fence = 8` from a stale queue read of `7`, both
> stamping short/expired leases so both *could* pass the lease branch. The first
> to reach the guard passes (`8 > 7`) and advances the committed fence to `8`.
> The second re-reads `8` under the guard and fails (`8 > 8` is false) → aborts.
> **No two committed claims can carry the same fence.** ∎

A fence-precondition failure is reported as a **new** non-retriable
`TransactionError::FenceTooLow { fence_key, new_fence, stored }` → MCP
`FAILED_PRECONDITION` (the correct caller response is *re-read the fence and
recompute*, not blind retry — distinct enough from a lost-claim `CasMismatch`
to warrant its own variant for debuggability, but the same non-retriable class).

### Backward compatibility

Additive only. The existing `claim_with_lease` / `compare_and_set_node` keep
their exact signatures and semantics (no fence). A **new** opt-in surface
carries fencing (see Piece 2's method, which bundles both). Existing callers
compile unchanged.

---

## 2. Piece 2 — DB-side lease-deadline computation

### Problem (design §5.3, N3, OQ8)

`lease_until = now() + TTL` is computed on the **executor's** wallclock and
stored verbatim. A skewed-*fast* executor writes a far-future `lease_until`; if
it then crashes, the run is un-stealable for a long time — a **liveness** hole
the DB clock does not bound.

### Approaches considered

- **(a) DB computes `lease_until = DB_now + ttl`** (caller passes only a TTL).
  Fully removes executor-clock dependence. **Chosen.**
- **(b) Clamp a caller-supplied `lease_until` to `DB_now + max_ttl`.** Keeps the
  caller's absolute deadline but caps the skew blast radius. Retained as the
  behavior of the plain method (which still takes a `Timestamp`) — *not* changed,
  for compat.
- **"Compute against `commit_HLC`"** (the design's literal phrasing) is **not
  implementable** for the same §0 reason as Piece 1(A): the stamped value is in
  the WAL-serialized map before the commit HLC's guard phase. We therefore
  compute against a **DB HLC `time::now()` taken at buffer-build time**, which is
  the same monotonic engine clock, within the transaction's own (microsecond-
  scale) duration of `commit_HLC`, and — being at most that sliver *earlier* —
  yields an if-anything-*shorter* lease, which is the **safe** direction for
  liveness (a lease can only expire too-early-by-a-sliver, never too-late).
  This deviation from the literal wording is deliberate and is called out for
  review.

### Chosen shape

The new fenced claim method takes `lease_ttl: Duration` and computes
`lease_until = time::now() + lease_ttl` **inside the engine**, at buffer-build
time, using the DB HLC. The executor's clock never enters the stored deadline.
`time::now()` is the engine's monotonic HLC "now" convention (the same one
`create_snapshot` uses); taking it at buffer-build time holds no locks
(buffer-build precedes the `current_timestamp → wal → historical` acquisition
chain), so it introduces no lock-order concern.

---

## 3. Piece 3 — compare-and-set op inside `apply_batch`

### Problem (design §5.3 "second, independent hole", OQ6)

Even with a sound fence, the step-record write must itself be gated on
ownership, or a zombie that lost its lease can still land a `create Step{output}`
— the `(Step, idem_key)` unique constraint enforces one-record-per-key but
**not** ownership, so the zombie can *poison* the memoized output a live
successor later replays. Gating it atomically requires a CAS/version-precondition
*inside the same `apply_batch`* as the `create Step`. `BatchOperation` today is
exactly six variants — no CAS op.

### Approaches considered

- **(1) New `compare_and_set_node` batch op variant.** A seventh
  `BatchOperation`. A step-record batch becomes
  `[compare_and_set_node(run, expected_version, …), create_node(Step…),
  create_edge(HAS_STEP…)]` committing all-or-nothing; if the run's head moved
  (a successor stole it), the CAS fails at the commit guard → the **whole batch
  aborts → zero writes** (the Step is never poisoned). **Chosen.**
- **(2) Per-op `expected_version` preconditions on the existing update/create
  ops.** More flexible, but adding an `Option<u64>` field to the existing struct
  variants **breaks byte-compatibility**: every Rust caller constructing
  `BatchOperation::UpdateNode { … }` by struct literal would have to add the new
  field. A *new* variant leaves all six existing variants byte-identical, so
  existing typed callers compile unchanged. Rejected for the compat cost.

### Chosen shape (1)

```
CompareAndSetNode {
    node_id: u64,           // committed node id (local '$refs' rejected, like update/delete)
    expected_version: u64,  // the version precondition
    properties: {…},        // full-replace map (matches the CAS primitive's semantics)
    valid_time: Option<String>,
    provenance: Option<ProvenanceRequest>,
}
```

Maps straight to `tx.compare_and_set_node_with_options`. Prevalidation reuses
`batch_committed_node` (committed-id-only, one-write-per-entity, batch-created-id
rejected). Execution appends a `{op:"compare_and_set_node", index, node_id,
version_id}` result; a `CasMismatch` at the commit guard flows through the
existing `BatchAbort` path → `FAILED_PRECONDITION`, and the existing
all-or-nothing drop-rollback guarantees **one failing precondition ⇒ zero
writes**. The op set stays byte-compatible: the six existing variants are
untouched; a batch that does not use the new variant behaves exactly as before.

v1 scope for the batch CAS op: full-replace (no PATCH), committed node ids only
(no lease/fence OR-branch inside the batch — plain version-precondition is what
closes the record-poisoning hole; the lease/fence branch lives on the claim
primitive). Edge CAS-in-batch and per-op preconditions are follow-ups.

---

## 4. Risk / edge-case list → concrete tests

| # | Risk / edge case | Test |
|---|---|---|
| 1 | Stale-fence steal reuses a fence on the **plain** primitive (documents the hole survives, plain semantics unchanged) | `plain_claim_still_admits_stale_fence_collision` |
| 2 | The **fenced** claim rejects the stale-fence steal (the RED→GREEN invariant) | `fenced_claim_prevents_stale_fence_collision` |
| 3 | First fenced claim on a never-claimed run (absent stored fence) succeeds | `fenced_claim_first_claim_absent_fence_succeeds` |
| 4 | Non-integer / absent stored fence treated as unclaimed (any `new_fence` admitted) | covered by #3 + a unit boundary test `fence_beats_stored` |
| 5 | Fenced claim still honors the lease/version gate (held lease + stale version + higher fence ⇒ still refused) | `fenced_claim_still_refused_when_lease_held` |
| 6 | Concurrent fenced steal of an expired lease: exactly one wins, loser aborts, winner's fence strictly greater | `concurrent_fenced_steal_one_winner_distinct_fence` |
| 7 | DB-computed `lease_until` ignores a wildly-skewed caller and is bounded by `DB_now + ttl` | `fenced_claim_lease_until_is_db_computed` |
| 8 | Batch CAS with a matching version commits the whole step-record batch | `batch_cas_matching_version_records_step` |
| 9 | Batch CAS with a stale version aborts the whole batch → zero writes (Step never poisoned) | `batch_cas_stale_version_aborts_whole_batch` |
| 10 | Batch CAS targeting a batch-created node id is rejected (v1 scope) | `batch_cas_of_batch_created_node_rejected` |
| 11 | Existing six-variant batches + existing `claim_with_lease` callers compile & behave unchanged | the pre-existing `tests/cas_lease.rs` + `apply_batch` suites stay green |

### Six-hats (condensed)

- **White:** WAL-before-apply ordering forces "reject, never mutate at apply"
  (verified in `apply.rs`/`commit`). The fence precondition and batch CAS both
  ride existing under-guard seams.
- **Black (risks):** the `commit_HLC`-vs-`buffer_HLC` deviation (Piece 2) — bounded
  and safe-direction, disclosed. `FenceTooLow` is a new error variant — additive,
  mapped once. Adding a `BatchOperation` variant is a compat break only for an
  *external exhaustive match* on the public enum (none in-tree); in-tree matches
  are updated.
- **Yellow:** the three holes the design sketch flagged as *not* closable by
  convention are now closable, all as additive, check-only, WAL-safe extensions.
- **Green (alternatives):** server-side fence counter store, per-op preconditions,
  `commit_HLC` finalization at apply — each rejected above with reasons; each is
  a viable future direction if the user prefers a different tradeoff.
- **Blue (process):** RED test first (collision reproduced), then GREEN per piece,
  gates run per-piece, draft PR up early.
