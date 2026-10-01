# Temporal Semantic Drift Alarms — Design (Issue #3367)

**Status:** Stage A (design + RED skeleton). Stage B (green implementation) pending.
**Feature cohort:** `semantic-temporal` (experimental; ADR-0050).
**Date:** 2026-07-18

> The database watches its own embedding evolution against declared thresholds and
> surfaces "this concept has changed meaning" as a durable, queryable, subscribable
> event — with the bi-temporal receipts (which versions, over which valid-time span,
> drifted how far) attached. Only an engine holding embedding *history* can detect
> that meaning moved; this productizes that latent asset.

---

## Brainstorming

- **Monitors as declarative rules in a registry.** A `DriftMonitor` is a small
  declaration: `{property_key, label?, entities?, metric, threshold, window,
  target, mode}`. The registry is off the data write path (a sidecar), so it adds
  no write overhead.
- **Drive on-write evaluation by subscribing to the EXISTING changefeed (#3216).**
  The changefeed already delivers entity-version changes *outside* every write-path
  lock. Subscribing an evaluator to it means there is **no new write-path hook** —
  zero commit overhead by construction, and the "async/background" AC is satisfied
  for free.
- **Alarms as first-class bi-temporal graph nodes** under a reserved label
  `__drift_alarm`. This makes `AS OF`-stability, WAL durability, changefeed delivery,
  and resolve-as-update all *inherited*, not re-implemented.
- **A PURE deterministic firing core** for exact-match fixtures — split from the
  async driver so the falsifiable firing rule is unit-testable with hand-computable
  vectors.
- **Label-centroid via component-wise arithmetic mean.**
- **Scheduled monitors via a background ticker.**
- **A bounded evaluation queue** that sheds on saturation with an observable counter.
- **Metric validated against the property's index metric** at creation.

## Reverse brainstorming (failure modes → inversions)

| Failure mode we could walk into | Inversion (what we do instead) |
|---|---|
| Tank write throughput by evaluating inline in commit. | **Never touch the write path.** Evaluate on a background thread driven by a best-effort changefeed subscriber that *sheds* on lag. |
| Alarms lie under `AS OF` because they live in a mutable side map overwritten by later writes. | **Append-only bi-temporal nodes.** Resolve = a new version, never a delete. |
| False positives from consecutive-snapshot / jittery endpoints. | **Exact two-endpoint rule** `distance(now, now−window)`, strictly `>` threshold. |
| Double-fire on every tick. | **Suppress** while an unresolved alarm for `(monitor, E)` exists; re-arm only after resolution. |
| Nondeterministic centroid (map iteration order / NaNs). | **Sorted entity iteration**, component-wise mean over entities that have the vector, skip missing, documented. |
| Crash on bad input. | **Validate at create** → #3234 structured errors. |
| Cost when the flag is off. | Whole module + accessors under `#[cfg(feature="semantic-temporal")]`; **no core edits**. |

## Six Thinking Hats

- **White (facts).** The temporal vector index already stores embedding history +
  drift metrics (`src/index/vector/temporal/`, `src/db/vector.rs`). The changefeed
  delivers entity-version changes *outside* write locks (`src/db/changefeed_sub.rs`,
  `src/core/changefeed*.rs`). `belief_revision` shows the gated `impl AletheiaDB`
  accessor idiom inside a feature-gated module file. Named snapshots (#3370) show the
  durable-sidecar pattern (`snapshots.json`).
- **Red (gut).** Materializing alarms as nodes feels like graph pollution — but the
  feature gate contains it and it buys correctness cheaply. The changefeed-subscriber
  evaluator feels elegant and safe.
- **Black (caution).** Background threads + write transactions risk lock-order
  violations and **reentrant** changefeed events (alarm creation itself emits a
  change — the subscriber MUST filter the reserved label to avoid a feedback loop).
  Timing-based tests are flaky. Cross-lane node-count pollution is a concern —
  mitigated because the CI clippy/test set excludes `semantic-temporal`, and
  flag-off = zero alarm nodes.
- **Yellow (upside).** Near-zero new storage code; all bi-temporal guarantees
  inherited; a category-defining capability; an exact-match-testable core.
- **Green (creativity).** Split the pure firing core from the async driver for
  determinism; expose `evaluate_drift_monitor_now` for scheduled cadence + tests;
  treat a label centroid as a *virtual entity*.
- **Blue (process).** TDD: pure firing fixtures first (100% exact match), then
  persistence against the real write path, then one live-driver e2e; gate on the full
  CI matrix incl. standalone `semantic-temporal`.

## Approaches considered

### 1. CHOSEN — Alarms as bi-temporal graph nodes + changefeed-subscriber evaluator
- **Pros:** inherits `AS OF`, durability, changefeed delivery, resolve-as-update for
  free; zero write-path hook; least new storage code.
- **Cons:** alarm nodes appear in node surfaces when the feature is active (gated +
  documented); requires background write-transaction discipline (own transaction,
  respects the lock-acquisition order, filters its own reserved-label events).

### 2. Dedicated in-memory/sidecar alarm store + synthetic changefeed publish
- **Pros:** no node-surface pollution.
- **Cons:** re-implements `AS OF`-stable append-only storage + durability by hand
  (exactly what AC5 tests — higher correctness risk); needs a new *core* changefeed
  publish seam (touches shared core). **Rejected.**

### 3. Inline on-commit evaluation writing alarms in the same transaction
- **Pros:** lowest latency, atomic.
- **Cons:** violates AC6 (async/shed) and the `<10%` write-overhead budget; lock-order
  hazard; back-pressures commits. **Rejected.**

## Firing rule (falsifiable, documented exactly)

**Per-entity.** For entity `E` and monitor `M`, let `e_now = embedding(E, now)` and
`e_past = embedding(E, now − window)` via the point-in-time history path. An alarm fires
**iff**:

1. both `e_now` and `e_past` exist, **and**
2. `distance_M(e_now, e_past) > threshold` (strict `>`), **and**
3. no unresolved alarm for `(M, E)` exists.

If `e_past` is missing or `e_now` is missing, the entity does **not** fire.

**The two endpoints are the literal reconstructions** (Fix-1, Issue #3367 correctness
review). `e_now` is `E`'s **current** embedding — the current-state value of the vector
property — together with its current `VersionId` (via `get_node`); an entity with no
current embedding (never had one, or the property was removed) does not fire. `e_past`
is the embedding **on record as of transaction-time `now − window`** (read at the current
valid coordinate), via `get_node_at_time(E, valid = now, tx = now − window)`; the version
current at that transaction time supplies the `from_version` ref. `e_past` is MISSING
exactly when `E` had no embedding on record at `now − window` — it was created after that
instant, or the property was absent then — matching "no embedding `window` ago → no fire".

**Reconstruction dimension — transaction-time as-of `now − window` (design decision
forced by the engine's bitemporal model).** The historical storage is
*system-time-versioned*: an update supersedes the prior version by **closing its
transaction interval** while its **valid interval stays open**. Empirically (verified
against `get_node_history` / `get_node_at_time`), a superseded version is therefore
invisible at `tx = now` for any past valid coordinate — a *valid-time* as-of `now − window`
at `tx = now` returns only the current version (if `now − window ≥ its valid_from`) or
nothing, **never** a superseded embedding. Reconstructing "what was the embedding `window`
ago" is thus only faithful along the **transaction-time** axis, so `e_past` is read at
`tx = now − window`. (The literal *valid-time* `embedding(E, now − window)` that the AC's
prose names is not recoverable in this engine; the transaction-time reading is the
correct realization of the same intent — "the embedding on record `window` ago" — and,
critically, actually fires on genuine drift.)

We deliberately do **not** substitute "the earliest version inside the window": that
heuristic picks a data-dependent write rather than the state `window` ago, stripping
`window` of its declared meaning and disagreeing whenever the entity's history spans the
window. Reconstructing at the exact coordinate `now − window` keeps `window` meaning "how
far back the past comparison anchor sits". The firing's `compared_now` is the evaluation
instant `now`; `compared_past` is the `now − window` transaction-time anchor actually
used; `from_version` / `to_version` name the versions that supplied `e_past` / `e_now`.

Because the past anchor is on the transaction axis, a deterministic fixture must spread
the *transaction* times of the versions (e.g. via an injected `SimulatedClock`) so that
`now − window` lands strictly between two commits; backdating *valid* time does not create
a readable past region under this engine's supersession model.

**Label-centroid.** `centroid(t) =` the component-wise arithmetic mean over all
entities carrying `M.label` that have the property at time `t` (iterated **sorted by
node id**; entities missing the vector are **skipped**). The now-centroid is taken over
**every** label member with a current embedding; the past-centroid over **every** member
whose embedding was on record at transaction-time `now − window` (same axis as the
per-entity rule). The two member sets need not coincide (a
member static over the window, or created inside it, contributes to only one) — the
centroid is emphatically **not** restricted to members with two in-window versions
(Fix-1 correctness review, lens1 MAJOR-2). For `Cosine` the mean is **NOT** renormalized
(documented); a zero-magnitude centroid (normalized members cancelling) is treated as
**no comparison → no fire** rather than a spurious max-drift artifact (lens1 MINOR-5).
The monitor fires iff `distance_M(centroid(now), centroid(now − window)) > threshold` and
no unresolved label alarm for `M` exists; a centroid with no contributing members does
not fire.

## Centroid & metric decisions

- **Centroid** = deterministic component-wise arithmetic mean (as above). Chosen over a
  medoid/normalized-mean because it is O(n) deterministic, hand-computable in fixtures,
  and needs no distance-matrix. Not renormalized (documented) so the rule is exact.
- **Metric** must be consistent with the property's vector-index `DistanceMetric`
  (`Cosine`/`Euclidean` map to `DriftMetric::Cosine`/`Euclidean`; `Angular` is a
  cosine-family refinement permitted on a cosine index). On mismatch, **reject** with
  #3234 `INVALID_ARGUMENT` at monitor creation.

## Risks / edge cases as test cases

| # | Test case | Defends AC |
|---|---|---|
| 1 | Below threshold → no alarm | AC2 (falsifiable rule) |
| 2 | Exactly at threshold → no alarm (strict `>`) | AC2 |
| 3 | Above threshold → exactly one alarm w/ correct distance + version refs | AC2, AC4 |
| 4 | Re-crossing → 2nd alarm only after resolution, none while unresolved | AC2, AC5 |
| 5 | No version in-window → no alarm | AC2 |
| 6 | No current embedding → no alarm | AC2 |
| 7 | Multi-entity mixed → exactly the above-threshold subset, no false positive on jitter | AC2, Success-metric (100% exact match) |
| 8 | Metric correctness cosine/euclidean/angular = documented distance | AC1 |
| 9 | Label centroid: population shift w/ no single entity over threshold → label alarm; deterministic centroid; empty label → none; single-entity centroid == that entity | AC3 |
| 10 | Unknown property → `INVALID_ARGUMENT` | AC8 |
| 11 | Non-positive threshold → `INVALID_ARGUMENT` | AC8 |
| 12 | Zero/negative window → `INVALID_ARGUMENT` | AC8 |
| 13 | Metric mismatch vs index → `INVALID_ARGUMENT` | AC1, AC8 |
| 14 | Fired alarm queryable via `query_drift_alarms` w/ all required fields | AC4 |
| 15 | Resolve marks resolved + unresolved filter excludes it + `AS OF` before resolution still shows unresolved | AC5 |
| 16 | Alarm not retroactively deleted by later entity writes | AC5 |
| 17 | Changefeed subscriber receives alarm-node `Created` event | AC4 |
| 18 | Monitor create/list/delete round-trip + delete removes from evaluation | AC1 |
| 19 | Bounded queue sheds on saturation → shed counter increments, commits never block | AC6 |
| 20 | Zero-cost-when-off (documented; enforced by `check-features` + CI set excluding the flag) | AC7 |

## AC → design mapping

| Issue #3367 requirement | Design element that satisfies it |
|---|---|
| **AC1** create/list/delete via Rust API + MCP; metric/threshold/window/scope/cadence | `DriftMonitorSpec` + gated `impl AletheiaDB { create/list/get/delete_drift_monitor }`; MCP tools designed (not registered — coordinator batch). |
| **AC2** falsifiable per-entity firing rule | Pure core `decide_entity_firing` + `metric_distance`, strict `>`, unresolved-suppression; fixture cases 1–7. |
| **AC3** aggregate label-centroid drift, deterministic | `DriftTarget::LabelCentroid` + `centroid()` (sorted component-wise mean, documented); case 9. |
| **AC4** durable first-class alarms, queryable, changefeed-delivered, version refs | Alarms as `__drift_alarm` bi-temporal nodes; `DriftAlarm` carries `from_version`/`to_version`; `query_drift_alarms`; changefeed `Created` delivery; cases 14, 17. |
| **AC5** temporally honest (never retro-deleted, resolve recorded, `AS OF` stable) | Append-only bi-temporal nodes; **resolution = a recorded bi-temporal update** (resolved flag + resolution tx-time), never a delete; cases 15, 16. |
| **AC6** write-path safety, async, shed-not-block | Changefeed-subscriber `DriftAlarmEngine` off the write path; bounded queue + `shed_count`; case 19. |
| **AC7** experimental flag, zero cost when off | Whole module + accessors under `semantic-temporal`; no core write-path edits; case 20. |
| **AC8** structured errors for invalid monitors | `invalid_argument()` → `QueryError::InvalidParameter` (#3234 `INVALID_ARGUMENT`); cases 10–13. |
| **Metric:** detection latency `<1s` p99 on-write | On-write changefeed subscriber evaluates promptly off-lock (Stage B benchmark). |
| **Metric:** `<10%` GroupCommit overhead w/ 10 monitors; `0%` flag-off | No write-path hook; flag-off compiles the module out. Benchmark-enforced in Stage B. |
| **Metric:** 100% exact-match fixture, no false positives on jitter | Deterministic pure core; cases 1–9 assert exact fired sets. |

**Two implementer-judgment areas resolved here** (the issue leaves them open):
- **Centroid** = component-wise arithmetic mean (documented, deterministic, not
  renormalized).
- **Alarm resolution** = a recorded bi-temporal update (a `resolved` flag + resolution
  transaction time on the alarm node), `AS OF`-stable — never a delete.

## Coordinator flags

1. **CI clippy/test feature set** (`config-toml,mcp-server,sharding-rpc,simulation`)
   does **NOT** include `semantic-temporal`, so this gated code is **not** linted or
   run by the default CI invocation. It must be linted separately via
   `cargo clippy --features semantic-temporal,mcp-server -- -D warnings` and via
   `just check-features` (each cohort flag compiled standalone, per ADR-0050).
2. **MCP surface is DESIGNED but NOT registered** (the coordinator owns the tool-registry
   batch). Intended tools, as a follow-up: `create_drift_monitor`, `list_drift_monitors`,
   `delete_drift_monitor`, `query_drift_alarms`, `resolve_drift_alarm` (writer-class for
   create/delete/resolve, reader-class for list/query), all under
   `#[cfg(all(feature = "mcp-server", feature = "semantic-temporal"))]`.
3. **New sidecar `drift_monitors.json`** for monitor durability, following the
   `snapshots.json` (#3370) / `schema_constraints.dat` (#3378) precedent. Monitors are
   the only new durable state; **alarms** reuse the existing node/WAL storage (no new
   format). No changes to any existing on-disk format.

## Stage boundary

- **Stage A (this change):** design doc, a compiling API skeleton (`todo!()` bodies), a
  RED test suite (inline pure-core fixtures + `tests/drift_alarm_e2e.rs`), clean build +
  clippy under `semantic-temporal[,mcp-server]`. No real firing/persistence logic.
- **Stage B (follow-up):** implement `metric_distance`, `centroid`,
  `decide_entity_firing`, `evaluate_monitor`, the registry + `drift_monitors.json`
  sidecar, alarm-node materialization, the `DriftAlarmEngine` changefeed subscriber +
  ticker + bounded/shedding queue, `.albk` round-trip for monitors, and a
  write-overhead benchmark. Then MCP registration (coordinator batch).

## Stage B2 result — background engine + write-overhead benchmark

- **`DriftAlarmEngine`** is implemented as a background driver with three thread
  roles: a **dispatcher** draining the existing changefeed subscription
  (filtered to the watched labels; the reserved `__drift_alarm` label is ignored
  so alarm materialization never re-triggers evaluation), a **worker** popping the
  bounded queue and running `evaluate_drift_monitor_now`, and a **ticker** for
  scheduled monitors. The evaluation queue is a bounded `sync_channel`; on
  saturation the producer `try_send` **sheds** (increments `shed_count()`) and
  never blocks the changefeed/commit path (AC6). `start`/`stop` spawn and join
  the threads cleanly (no panics on drop); the engine's own locks are leaves (the
  worker never holds them while calling into the DB), so no new edge is added to
  the documented lock-acquisition order. v1 snapshots the monitor set at `start`
  (monitors added later are picked up on restart — a documented follow-up).
- **Write-overhead benchmark** (`benches/drift_alarm_overhead.rs`, gated
  `semantic-temporal`) compares GroupCommit commit throughput baseline vs 10
  active on-write monitors + a running engine. The pure write-path cost (queue
  shedding under load, the steady-state model) measured **+0.3%..+6.7%** vs
  baseline across short runs — statistically indistinguishable from zero, since
  the 10 ms GroupCommit fsync window dominates each ~11.6 ms commit and the only
  added per-commit cost is one changefeed subscriber push. This is **well under
  the 10% AC**, and **flag-off is 0%** (the whole module compiles out). An
  actively-evaluating variant is reported for completeness only: its cost is the
  background evaluator's O(population) read contention (data-dependent, not a
  fixed per-commit write cost), not the AC metric.
- **Deterministic shed test.** Case 19 (`saturated_queue_sheds_without_blocking_commits`)
  is made deterministic via a `#[doc(hidden)]` `set_evaluation_paused` gate: with
  the worker frozen, a capacity-1 queue provably saturates and sheds while every
  commit still succeeds — a faithful, race-free model of the saturated-evaluator
  scenario AC6 governs (a stalled evaluator *is* saturation). The original
  formulation raced the evaluator against the commit rate and additionally drove a
  single node past the 1000-version storage cap; both are fixed without weakening
  what the test proves.

## Red phase evidence

The pure-core functions (`metric_distance`, `centroid`, `decide_entity_firing`,
`evaluate_monitor`) and the `AletheiaDB` accessors are `todo!()` in Stage A, so every
behavioral test panics. Raw output from
`cargo test --features semantic-temporal --lib experimental::temporal::drift_alarm`
(inline pure-core suite):

```text
thread '...::metric_distance_euclidean_is_l2' panicked at src/experimental/temporal/drift_alarm.rs:317:5:
not yet implemented: Stage B (#3367): metric distance computation
thread '...::evaluate_monitor_smoke_uses_now' panicked at src/experimental/temporal/drift_alarm.rs:364:5:
not yet implemented: Stage B (#3367): monitor evaluation over temporal vector history

failures:
    ...::centroid_is_component_wise_arithmetic_mean_not_renormalized
    ...::centroid_of_empty_is_none
    ...::centroid_single_entity_equals_that_entity
    ...::evaluate_monitor_smoke_uses_now
    ...::firing_above_threshold_fires_with_distance
    ...::firing_below_threshold_does_not_fire
    ...::firing_exactly_at_threshold_does_not_fire_strict_gt
    ...::firing_no_current_embedding_does_not_fire
    ...::firing_no_past_embedding_does_not_fire
    ...::firing_suppressed_while_unresolved_alarm_exists
    ...::metric_distance_angular_orthogonal_is_half_pi
    ...::metric_distance_cosine_identical_is_zero
    ...::metric_distance_cosine_orthogonal_is_one
    ...::metric_distance_euclidean_is_l2

test result: FAILED. 3 passed; 14 failed; 0 ignored; 0 measured; 4396 filtered out
```

The gated integration suite (`tests/drift_alarm_e2e.rs`, cases 10–20) likewise panics on
the `todo!()` accessors — it compiles under `--features semantic-temporal` and fails at
runtime, as required for a RED phase. Sample:

```text
thread 'create_monitor_metric_mismatch_is_invalid_argument' panicked at src/experimental/temporal/drift_alarm.rs:438:9:
not yet implemented: Stage B (#3367): validate + register drift monitor
...
test result: FAILED. 0 passed; 13 failed; 0 ignored; 0 measured; 0 filtered out
```

No pre-commit / pre-push hook is installed in the build environment (only
`.sample` hooks; no `core.hooksPath`), so the failing RED tests are committed and
pushed as-is — none are `#[ignore]`d, and no assertion is weakened. The default CI
test/clippy set excludes `semantic-temporal`, so these tests neither compile nor run
under default CI; they are exercised only via `cargo test --features
semantic-temporal` (Stage B turns them green).

The three non-behavioral anchor tests (`monitor_id_round_trips`,
`drift_target_tokens_are_stable`, `filter_defaults_and_for_monitor`) pass — they
exercise only the concrete newtype/enum/default code, not the `todo!()` core.
