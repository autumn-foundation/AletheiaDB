# WAL Abort Framing — recovery must not reapply post-WAL-rejected transactions (Issue #3413)

**Status:** DRAFT (no on-disk WAL format change — see §7; user merge = sign-off).
**Lane:** core-storage. **Branch:** `claude/wal-abort-framing-doef9j`.

## 1. Problem

Issue #3413 originally described two crash-atomicity defects: (P1) prefix-replay
of a torn multi-op batch, and (P2) per-entry timestamp bisection. Both were
closed by the **commit framing** already on trunk — a committing
`WriteTransaction` writes `[BeginTx, ..data ops.., CommitTx]` as one atomic,
contiguous LSN band, and `resolve_transaction_frames`
(`src/storage/recovery.rs`) discards any frame whose terminal `CommitTx` marker
never became durable, re-stamping every op of a committed frame with the
marker's single authoritative commit timestamp.

The **remaining, still-open gap** — the subject of this doc, and the load-bearing
caveat documented by merged PR #3755 (DBOS Phase 3e) — is **abort framing**:

> A transaction whose **complete** `[BeginTx, ..ops.., CommitTx]` frame is
> durably written **and then rejected** at the commit guard leaves a fully valid
> frame on disk. Because the WAL write precedes the guard check, and recovery
> replays frames without re-running preconditions, a transaction that is
> correctly refused **at runtime** is silently **reapplied on crash recovery**.

This is a genuine correctness hole: a rejected zombie write (a lost CAS/lease
claim, a stale fence, an orphaning delete) resurrects after a crash, defeating
the very guard that refused it live.

## 2. Where the rejection happens (commit path today)

`WriteTransaction::commit_with_timestamp_inner` (`src/api/transaction/write/mod.rs`):

```
validate()                         // referential integrity        [pre-WAL]
detect_conflicts()                 // SI write-write + pure-CAS fast-path [pre-WAL]
check_constraints()  (reservation) // uniqueness + schema #3378     [pre-WAL]
── under current_timestamp lock ───────────────────────────────────────────
  assign commit_timestamp (HLC)
  log_operations_to_wal()          // append [BeginTx, ..ops.., CommitTx]
  wal.commit() + wait_for_flush()  // DURABLE HERE
── release current_timestamp ──────────────────────────────────────────────
apply_changes():                   // acquires historical.write()
  detect_delete_orphan_write_skew()        // ← REJECTS post-WAL  (#3416)
  detect_create_edge_dangling_endpoint()   // ← REJECTS post-WAL  (#3416)
  detect_cas_precondition_violations()     // ← REJECTS post-WAL  (#3577/#3755)
  apply each op (commit_timestamp: None)
finalize_current_commit_timestamps()       // makes versions visible
```

## 3. Rejected-path inventory (every post-WAL-write rejection)

| # | Site (in `apply_changes`, under `historical.write()`) | Error | Exposed via | Pre-WAL fast-path? |
|---|---|---|---|---|
| R1 | `detect_cas_precondition_violations` — version mismatch | `CasMismatch` | `compare_and_set_node/edge`, `apply_batch` CAS op | **Pure CAS only**: `conflict::detect_conflicts` rejects a *single-threaded stale pure CAS* pre-WAL. A **concurrent** pure-CAS loser, or **any lease/fenced claim**, reaches here. |
| R2 | `detect_cas_precondition_violations` — lease branch | `CasMismatch` | `claim_with_lease` | No — lease claims are excluded from the fast-path. |
| R3 | `detect_cas_precondition_violations` — fence branch | `FenceTooLow` | `claim_with_lease_fenced` | No — fenced claims excluded from the fast-path. **Deterministic single-threaded reproduction** (see §5). |
| R4 | `detect_delete_orphan_write_skew` — delete/retract node orphaning a concurrently-created edge | `ValidationFailed` | `delete_node`/`retract_node` | No — write-skew, disjoint write sets escape first-committer-wins. |
| R5 | `detect_create_edge_dangling_endpoint` — create-edge whose endpoint a concurrent tx deleted | `ValidationFailed` | `create_edge` | No — same. |

**Not part of the gap (already pre-WAL, verified):**

- **Uniqueness constraints (#3218)** and **schema constraints (#3378)** are
  enforced by `check_constraints` at `commit_with_timestamp_inner` *before* the
  `current_timestamp` lock and the WAL append (the reservation guard). A
  violation aborts with zero WAL writes. There is **no** second constraint
  enforcement inside `apply_changes`.
- **SI write-write conflicts** (`detect_conflicts`) and **referential-integrity
  validation** (`validate`) are pre-WAL.
- **Storage errors during the apply loop** (e.g. `InconsistentState`) are *not*
  precondition rejections — they are failures of an already-validly-committed
  transaction, and recovery *correctly* re-materializes such a frame. Out of
  scope (not a "rejected zombie write").

**Key structural fact exploited by the fix:** every check in R1–R5 reads
**only current storage** (`tx.current.get_node/get_edge/get_outgoing_edges/…`)
and this transaction's own buffer. **None reads `historical` storage.** They
sit under `historical.write()` today purely to borrow it as the
commit-serialization barrier, not because they need historical data.

## 4. Chosen mechanism — **Guard-before-log** (reorder under the commit clock)

Move R1–R5 to run **before** `log_operations_to_wal`, and **extend the
`current_timestamp` critical section to span apply + finalize**, so the whole
commit — precondition check → WAL append → durability → apply → finalize —
is one serialized unit. A transaction rejected by the checks **never appends a
WAL frame**, so there is nothing for recovery to reapply. Nothing on disk
changes.

```
── acquire current_timestamp lock ──────────────────────────────────────────
  assign commit_timestamp (HLC)
  detect_delete_orphan_write_skew()        ┐  R4  ] read CURRENT storage only
  detect_create_edge_dangling_endpoint()   ├─ R5  ] (no historical); reject ⇒
  detect_cas_precondition_violations(ts)   ┘  R1–R3] return Err, NO WAL append
  log_operations_to_wal()   // append [BeginTx, ..ops.., CommitTx]
  wal.commit() + wait_for_flush()          // DURABLE
  apply_changes()                          // historical.write(); NO checks now
  finalize_current_commit_timestamps()
── release current_timestamp lock ──────────────────────────────────────────
(vector notify, constraint-guard commit, register_commit, changefeed broadcast)
```

### 4.1 Why this is correct (check stays valid through apply)

The checks read current storage, which is mutated **only** by `apply_changes` +
`finalize`. The invariant we need is: *between a transaction's check and its
apply, no other transaction applies.* Holding `current_timestamp` across
`check → append → flush → apply → finalize`, **for every commit**, provides
exactly that: no other committer can be in its own check/append/apply while one
holds the lock. Therefore a check that passes at check-time is still valid at
apply-time within the same held section. Concretely for a fenced-claim race:

- T1 holds `current_timestamp`; checks (fence stored=10, new=11 > 10 ✓); appends;
  flushes; applies (fence→11); finalizes; releases.
- T2 (blocked on `current_timestamp` the whole time) then checks: fence stored=11
  (T1 applied); new=11 ⊁ 11 ⇒ `FenceTooLow` ⇒ **returns with no WAL frame**.

Exactly one durable winner; the loser writes nothing. Symmetric for R4/R5 in
both delete-first and create-first orderings.

### 4.2 Why this respects the documented lock order

CLAUDE.md order: `current_timestamp(1) → wal(2) → historical(3) → …`. The fix
keeps every acquisition ordered:

- The checks acquire **no** ordered primitive — they read leaf structures
  (current-storage `DashMap`, adjacency) while holding only `current_timestamp(1)`.
- `wal(2)` is appended while holding only `current_timestamp(1)` (`2 > 1`).
- `historical(3)` is acquired (inside `apply_changes`) while holding only
  `current_timestamp(1)` (`3 > 1`).

`wal` is **never** appended while `historical` is held — the inversion the WAL
subagent analysis explicitly warned against. The order is unchanged; only the
*duration* of the `current_timestamp` hold grows (now spans apply + finalize).

### 4.3 Cost / concurrency

`current_timestamp` is *already* held across `append → wal.commit() →
wait_for_flush` (the dominant fsync cost), which already serializes committers
(GroupCommit cross-committer fsync batching is already collapsed to one
transaction per epoch — a pre-existing property, **not changed or fixed here**).
This fix additionally holds it across `apply_changes` + `finalize`
(microsecond-scale, in-memory). The only measurable loss is the small overlap
where one transaction's apply currently runs concurrently with the *next*
transaction's append — now serialized. Historical **readers** are unaffected:
`historical.write()` is still held only during apply (µs), never across the
flush. Recovery is untouched, so the `<5s` medium-dataset recovery target and
`resolve_transaction_frames` idempotency are unchanged. A commit-throughput
micro-benchmark is deferred (quantify the lost apply/next-append overlap).

### 4.4 Interaction: the lost-write persist race is subsumed (Issue #3413 / 2026-07-13)

Holding `current_timestamp` across apply has a beneficial side effect.
`persist_indexes()` briefly acquires `current_timestamp` to read the WAL frontier
and the in-flight set consistently (`src/db/admin.rs`). It therefore now
**serializes behind any in-flight commit's apply**: it can never snapshot a
durable-but-unapplied write, and two commits can no longer overlap (the second
blocks on the clock until the first has fully applied). This makes the lost-write
persist race (durable-but-unapplied write dropped by a racing persist)
**structurally impossible** — the in-flight-LSN *applied watermark* added on
2026-07-13 becomes belt-and-suspenders (in-flight is always empty when persist
observes it). The watermark code is retained (harmless); the
`tests/lost_write_persist_race.rs` regression tests are updated to pin the
*stronger* invariant (persist / a second commit BLOCKS behind a parked commit;
nothing is lost or duplicated across a crash) rather than the now-impossible
race. Cost: a periodic/background `persist_indexes()` may wait one in-flight
commit's apply (µs) for the clock — negligible.

### 4.5 Test seam repositioned (pre-apply → pre-commit-clock)

The `#[cfg(test)]` pre-apply hook (which drove a concurrent commit in the
old "after WAL, before apply" window to exercise the #3416 write-skew re-checks)
is repositioned to fire **before** `current_timestamp` is acquired (a
`pre_commit_clock` hook). Firing after WAL would now deadlock — the concurrent
committer would block on the `current_timestamp` the victim holds across apply —
whereas firing before the victim takes the clock lets the concurrent commit run,
after which the victim aborts at its own pre-WAL re-check. The always-compiled
`race_seam` (lost-write tests) still fires at the durable-but-unapplied point;
its body only reads/persists (needs `historical.read()`, not
`current_timestamp`), so the park serializes rather than deadlocks.

## 5. Test strategy (TDD, red first)

**Primary RED (deterministic, single-threaded):** a **fenced claim** with
`new_fence ≤ stored_fence` passes the claim gate (version match) but fails the
fence gate (R3) under the commit guard — and fenced claims are excluded from the
pre-WAL fast-path, so today it appends a durable `[BeginTx, UpdateNode,
CommitTx]` frame. Test: create node with `fence=10`; issue
`claim_with_lease_fenced(new_fence=5)` (asserts `FenceTooLow`); drop db; delete
`indexes/` to force full WAL replay; reopen; assert the node's fence is still
`10` and the claim's owner property is absent. **Today: RED** (replay applies the
rejected claim → fence=5). **After fix: GREEN** (no frame appended).

**Companion RED (concurrent write-skew, R4/R5):** using the existing
`race_seam`/`commit_test_hooks` seam (or two sequential snapshot-crossing
transactions), drive a delete-orphan / dangling-endpoint rejection whose durable
frame replays today; assert it does not resurface after forced replay.

**Interruption-point tests:** crash (drop + forced replay) at each boundary —
(a) before the checks, (b) after a rejected check (no frame), (c) after a durable
committed frame (must survive), (d) torn `CommitTx` (benign tail, prior tx
survives). Reuse the `wal_tx_framing.rs` truncation harness.

**Regression:** the full `wal_tx_framing.rs`, `recovery/*`, `cas_lease`,
`dbos_phase3e_fence`, and the #3416 write-skew tests must stay green (the #3416
sequential tests observe the *same* `ValidationFailed`, now raised pre-WAL).

## 6. Adversarial review lenses (mandatory)

- **Concurrency:** fenced-claim / CAS / write-skew races across threads produce
  exactly one durable winner and zero phantom frames; no lock-order inversion;
  no deadlock with the background flush thread (which never touches
  `historical`); the `race_seam` park (now under `current_timestamp`) cannot
  self-deadlock a *non-committing* racer (persist needs `historical.read()`, not
  `current_timestamp`).
- **Recovery:** replay behavior is byte-identical for all committed frames;
  no rejected frame is ever produced to replay; legacy/pre-v7 and
  KEYVERSIONED-encrypted (v16) segments read unchanged.

## 7. Invariants preserved

- **No on-disk WAL format change**; no new `WalOperation`. v16 KEYVERSIONED
  encrypted segment compatibility, the encrypted-version even-parity invariant,
  and the framing/contiguity resolver are all untouched.
- Replay idempotency, GroupCommit batching semantics (unchanged — already
  serial), `<5s` recovery target.
- Backward compatible: old segments replay identically; this is a **live
  commit-path** change only.

## 8. Follow-ups / residue

- Update the stale #3413 caveats now that the runtime rejection is crash-durable:
  `claim_with_lease_fenced` rustdoc (`src/db/ops.rs`), `cas.rs` module docs, the
  `apply_changes` comment block, and the `cas_recovery.rs` test note.
- Commit-throughput micro-benchmark for the lost apply/next-append overlap.
