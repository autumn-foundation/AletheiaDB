# Knowledge Half-Life Analytics (Issue #3377)

**Status:** Stage A (design + skeleton + red tests). Stage B (green implementation) pending.
**Feature gate:** `semantic-temporal` (experimental, ADR-0050).
**Module:** `src/experimental/temporal/half_life.rs`.
**Prior art built upon:** belief-revision audit (Issue #3362,
`src/experimental/temporal/belief_revision.rs`) — reused as the event oracle.

---

## 1. Problem

Every knowledge base silently decays and nobody measures the decay rate. A
person's `email` changes every couple of years; a company's `stock_price`
changes hourly; a country's `capital` almost never — yet every retrieval system
treats a fact recorded 18 months ago about each identically, because no database
knows *how long facts of a given kind tend to stay true*. AletheiaDB is the only
engine that records the raw material to compute this: every fact's full
supersession history across valid and transaction time. **Knowledge half-life
analytics** runs survival analysis over that history to answer *how long does a
fact of this kind survive before it is superseded?* — yielding per-cohort
volatility statistics, per-fact freshness scores, and staleness inventories.

Read-only analytics over already-stored history: no writes, no storage-format
change, no write-path hook.

---

## 2. Brainstorming

Survival analysis over bi-temporal version history: each fact version's
valid-time lifespan is one observation. Kaplan–Meier estimator (non-parametric,
censoring-aware) → half-life = KM median survival. The terminating-event vs
censoring decision reuses #3362 `RevisionClass` (WorldChange / Retraction
terminate a lifespan; Correction / Reaffirmation continue it; an open valid
interval is right-censored). Cohorts: node label, edge type, (label,
property_key). Per-fact freshness score = age in half-lives + survival
probability from a cached KM curve. Staleness inventory = aged current facts
filtered by threshold, paginated. As-of transaction-time scoping makes reports
replayable and pinnable (#3370). Bounded capped scan
(`max_schema_as_of_entities`), `sampled` flag, cold-tier coverage caveat
(#3238). In-memory recomputable cohort-stats cache (no durable format, no
write-path hook) backs the sub-millisecond freshness path.

---

## 3. Reverse Brainstorming (failure → inversion)

| Failure mode | Inversion (design response) |
|---|---|
| Biased half-life that ignores censoring | KM right-censors still-valid facts (open interval → censored observation). |
| Spurious numbers on tiny cohorts | Documented `insufficient_data` floor (`MIN_EVENTS_FOR_ESTIMATE`); never emit a number below it. |
| Conflating tx-time corrections with real world changes | Reuse `RevisionClass`; only WorldChange/Retraction end a life. |
| Silently mutating reads with freshness | Freshness is an opt-in annotation / on-demand call, never default. |
| Unbounded scan tanking latency | Cap + `sampled` + cohort cache. |
| Non-reproducible runs | As-of tx-time makes runs replayable; pure estimator is deterministic. |
| Cost when the flag is off | Gated module + accessors, no write-path hook, lazy cache. |

---

## 4. Six Thinking Hats

- **White (facts / substrate):**
  `BiTemporalInterval`/`TimeRange::duration_micros() -> Option<i64>` (the
  censoring primitive; `None` = open = censored),
  `RevisionClass` classifier (`belief_revision.rs`),
  `temporal_extent_by_label` scan + lock discipline (`src/db/extent.rs`),
  `versioned_node_ids`/`versioned_edge_ids` + `max_schema_as_of_entities`
  (`src/storage/historical/mod.rs`), `SimulatedClock`
  (`src/simulation/clock.rs`, `simulation` feature) for deterministic lifespans.
- **Red (gut):** KM is the right rigor; reuse `RevisionClass` (don't reinvent the
  event oracle); an in-memory cache fits recomputable analytics.
- **Black (caution):** property-cohort events are per-property (RevisionClass is
  entity-level — needs per-property diffing); cache-invalidation vs
  zero-write-cost tension (resolve: lazy compute, no write hook); cold-tier
  coverage gaps; large-cohort scan latency; f64 KM edge cases (ties,
  all-censored).
- **Yellow (optimism):** category-defining feature; all substrate present except
  KM; clean #3362 reuse; the pure estimator is trivially testable.
- **Green (creative):** separate the **pure** KM estimator
  (`&[Observation] -> KmCurve`) from the cohort scan and the freshness/staleness
  surfaces (the `summarize` / `survival_probability` seam).
- **Blue (process):** TDD — pure KM fixtures first (planted half-life + 30%
  censoring), then cohort scan on real history via `SimulatedClock`, then
  freshness/staleness; gate on the full CI matrix incl. standalone
  `semantic-temporal`.

---

## 5. Approaches

1. **CHOSEN — Kaplan–Meier over per-version valid-time lifespans; event oracle =
   #3362 `RevisionClass`; in-memory recomputable cohort cache.**
   *Pros:* correct under censoring (meets the ±10% / 30%-censored metric); reuses
   the deterministic world-change-vs-correction classifier (no NLP); no durable
   format; zero write-path hook.
   *Cons:* O(scan) per cohort (capped + cached); property-cohort needs
   per-property event derivation.
2. Parametric exponential MLE (half-life = ln2/λ̂). *Pros:* closed-form, cheap.
   *Cons:* assumes memoryless lifespans (usually false), weaker under 30%
   censoring. **Rejected as primary** — kept only as the freshness
   survival-probability **fallback** `S(age) = 0.5^(age/half_life)`.
3. Naive mean/median of completed lifespans, dropping censored. **Rejected:**
   severely biased (drops still-alive facts), violates AC1 censoring-awareness
   and the success metric.

---

## 6. Statistical Decisions (pinned)

- **half-life = KM median** = the smallest `t` with `Ŝ(t) ≤ 0.5`. If `Ŝ` never
  reaches 0.5 (heavy censoring) → half-life **undefined**, reported with the
  survival-at-max and a caveat **distinct** from `insufficient_data`
  (`half_life == None` while `insufficient_data == false`).
- **dispersion = KM IQR** — the (25th, 75th)-percentile survival times; report
  observation / event / censored counts alongside.
- **`insufficient_data` floor** = `MIN_EVENTS_FOR_ESTIMATE` (= 20). The ≥100-obs
  figure in the success metric is the *accuracy* target, not the *refusal* floor.
  Below the floor → a structured `insufficient_data` result (a **normal** result,
  never a number, never an error).
- **one observation** = one version's valid-time lifespan, terminated by the next
  WorldChange/Retraction; Correction/Reaffirmation continue it; an open valid
  interval = right-censored.
- **freshness:** `age` = current valid age; `age_in_half_lives = age / half_life`
  (`None` if no half-life); `survival_probability = Ŝ_cohort(age)` read from the
  cached curve, else exponential fallback.
- **as_of_transaction_time:** consider only versions as recorded by that
  transaction time (extend #3362's audit as-of to the cohort scan).

---

## 7. Public API (Rust)

Module `aletheiadb::experimental::temporal::half_life` (gated `semantic-temporal`).

```rust
pub enum Cohort {
    NodeLabel(String),
    EdgeType(String),
    NodeProperty { label: String, key: String },
}

pub struct HalfLifeOptions {
    pub as_of_transaction_time: Option<Timestamp>, // AC6 replayable
    pub min_events: usize,                          // AC3 floor (default 20)
    pub max_entities: usize,                        // AC7 cap (default 50_000)
}

pub struct VolatilityStats {
    pub cohort: Cohort,
    pub half_life: Option<Duration>,          // KM median
    pub iqr: Option<(Duration, Duration)>,    // KM 25th/75th pctile
    pub observation_count: usize,
    pub event_count: usize,
    pub censored_count: usize,
    pub sampled: bool,                        // AC7 truncation
    pub insufficient_data: bool,             // AC3
}

pub struct FreshnessScore {
    pub entity: EntityId,
    pub cohort: Cohort,
    pub age: Duration,
    pub age_in_half_lives: Option<f64>,
    pub survival_probability: Option<f64>,
}

pub enum StalenessThreshold { AbsoluteAge(Duration), HalfLives(f64) }
pub struct StalenessEntry { entity, age, age_in_half_lives, survival_probability }
pub struct StalenessPage { entries, total_matching, sampled, next_offset }

// PURE estimator (no I/O):
pub struct Observation { pub duration_micros: i64, pub censored: bool }
pub struct KmCurve { pub steps: Vec<(i64, f64)> }
impl KmCurve { fn median(&self) -> Option<i64>; fn percentile(&self, p: f64) -> Option<i64>; fn survival_at(&self, t: i64) -> f64; }
pub fn kaplan_meier(obs: &[Observation]) -> KmCurve;
pub fn summarize(cohort: Cohort, obs: &[Observation], min_events: usize) -> VolatilityStats;
pub fn survival_probability(age: Duration, half_life: Option<Duration>, curve: Option<&KmCurve>) -> Option<f64>;

// Gated DB accessors:
impl AletheiaDB {
    pub fn knowledge_half_life(&self, cohort: Cohort, opts: &HalfLifeOptions) -> Result<VolatilityStats>;
    pub fn fact_freshness(&self, entity: EntityId, opts: &HalfLifeOptions) -> Result<FreshnessScore>;
    pub fn staleness_inventory(&self, cohort: Cohort, threshold: StalenessThreshold,
                               offset: usize, limit: usize, opts: &HalfLifeOptions) -> Result<StalenessPage>;
}
```

An in-memory `CohortStatsCache` (recomputable, **never persisted**, off the write
path) backs the sub-millisecond freshness path. Stage A ships it as a stub; Stage
B fleshes out lazy population/invalidation. The skeleton does **not** add a field
to `AletheiaDB` — Stage B decides whether the cache lives as a gated field
(mirroring drift_alarm's gated field) or is threaded through the accessors; the
Stage A `todo!()` bodies need no such field.

### Type notes forced by the real APIs

- Durations use **`std::time::Duration`** — there is no `core::Duration` type in
  the crate; the temporal primitives speak `i64` microseconds
  (`TimeRange::duration_micros() -> Option<i64>`), which the pure estimator
  consumes directly and the surface converts to `Duration`.
- The censoring primitive is exactly `TimeRange::duration_micros()`: `None`
  (open interval) → censored, `Some(d)` → completed lifespan of `d` µs.

---

## 8. MCP Surface (designed, NOT registered in Stage A)

Three read-only (`reader`-class) tools, mirroring the existing temporal-tool
envelopes; **no tool-registry changes land in this stage** (skeleton only):

- `knowledge_half_life` — args `{ cohort_kind, label?, edge_type?, property_key?,
  as_of_transaction_time?, min_events?, max_entities? }`; returns `VolatilityStats`
  (+ the #3238-style coverage-window disclosure).
- `fact_freshness` — args `{ entity_kind, id }`; returns `FreshnessScore`. Also
  designed as an **opt-in annotation** on existing MCP reads (never default,
  never silently altering a response — AC4).
- `staleness_inventory` — args `{ cohort…, threshold_kind, threshold_value,
  offset?, limit? }`; returns `StalenessPage` with `next_offset`/`total_matching`
  per the offset/limit list convention.

Errors use the #3234 structured codes (`INVALID_ARGUMENT` for a bad
cohort/threshold, `NOT_FOUND` for an unknown entity in `fact_freshness`), all
`retriable: false`.

---

## 9. Errors

- Unknown entity (`fact_freshness`) → `NOT_FOUND` (storage error), non-retriable.
- Invalid cohort / threshold / paging argument → `INVALID_ARGUMENT`
  (`QueryError::InvalidParameter`), non-retriable — the belief-revision
  `invalid_argument` helper pattern.
- `insufficient_data` is **not** an error — it is a normal `VolatilityStats`
  result with `insufficient_data == true` (AC3).

---

## 10. Risks / edge-cases as tests

| # | Risk / edge case | Test (RED in Stage A) |
|---|---|---|
| 1 | Censoring bias | `km_recovers_planted_half_life_within_tolerance` (≥100 obs, 30% deterministic censoring, ±10%) |
| 2 | All-censored ≠ insufficient | `all_censored_has_no_median` (median `None`, `insufficient_data == false`) |
| 3 | Tiny cohort → spurious number | `below_floor_is_insufficient_data` |
| 4 | Correction miscounted as change | `correction_does_not_count_as_end_of_life` (e2e, SimulatedClock) |
| 5 | Retraction not counted as EOL | `retraction_is_end_of_life` (e2e) |
| 6 | Freshness math | `freshness_survival_probability_reads_curve` (pure) + `fact_freshness_reports_age_in_half_lives` (e2e) |
| 7 | Staleness filter/paging/sampled | `staleness_inventory_filters_paginates_and_flags_sampled` (e2e) |
| 8 | As-of excludes later revisions | `as_of_transaction_time_excludes_later_revisions` (e2e) |
| 9 | Scan cap → sampled | `scan_cap_sets_sampled_flag` (e2e) |
| 10 | Property-cohort per-property events | `property_cohort_classifies_per_property` (e2e) |
| 11 | Median = smallest t with S≤0.5, IQR, ties | `median_and_percentiles_over_explicit_curve` + `tied_event_times_fold_into_one_step` (pure) |
| 12 | Empty cohort → no panic | `empty_cohort_is_insufficient_data` (pure) |
| 13 | Single-observation cohort | `single_event_curve_drops_to_zero` (pure) |
| 14 | Node-label AND edge-type cohorts | `node_label_and_edge_type_cohorts_both_work` (e2e) |
| 15 | Zero-cost-when-off | Not a runtime test — enforced by `check-features` + the CI job that builds *without* `semantic-temporal` (compile-out guarantee). Documented in the module's `tests`. |

Pure tests are inline in `src/experimental/temporal/half_life.rs`; e2e tests are
in `tests/half_life_e2e.rs` (gated `all(semantic-temporal, simulation)`).

---

## 11. AC → design mapping

| AC | Where |
|---|---|
| AC1 volatility per label/edge-type/property, median + count + dispersion, censoring-aware, fixture recovers planted lifetimes | `Cohort` (3 granularities), `VolatilityStats` (half_life/iqr/counts), `kaplan_meier`, test 1 |
| AC2 retractions = EOL; correction-of-record distinguished from world-change; "superseded" explicit & reproducible | `RevisionClass` reuse (§6, §2b), tests 4/5; pure deterministic estimator |
| AC3 `insufficient_data`, never spurious | `MIN_EVENTS_FOR_ESTIMATE`, `summarize`, tests 3/12 |
| AC4 freshness score, on-demand + opt-in annotation, never silent | `FreshnessScore`, `fact_freshness`, `survival_probability`, tests 6 |
| AC5 staleness inventory, threshold (absolute/half-lives), paginated | `StalenessThreshold`, `StalenessPage`, `staleness_inventory`, test 7 |
| AC6 as-of past transaction time, replayable/pinnable (#3370) | `HalfLifeOptions::as_of_transaction_time`, test 8 |
| AC7 bounded, capped, truncation flagged, no write-path impact | `max_entities`, `sampled`, tests 9; read-only module |
| AC8 coverage caveats explicit (hot + restored; cold per #3238) | Module docs + response disclosure (§1, §6 Six-Hats White) |
| AC9 experimental flag + graduation checklist | `semantic-temporal` gate; graduation per ADR-0050 |

---

## 12. Coordinator flags

- **`RevisionClass::classify` visibility:** the classifier is a **module-private
  free `fn classify(...)`** in `belief_revision.rs` (not `pub`/`pub(crate)`).
  Stage B must either promote it to `pub(crate)` (a one-line change the #3362
  owner should sign off) or duplicate the ~5-line precedence rule with a doc
  cross-reference. `RevisionClass` itself is already `pub`.
- **MCP tools are designed, not registered** in Stage A (no tool-registry
  changes; skeleton only). Registration lands in a later stage.
- **`semantic-temporal` is absent from the CI clippy feature set**
  (`.github/workflows/ci.yml` clippy job omits it), so the gated module is **not**
  linted by the main CI clippy job — lint it manually with
  `cargo clippy --features semantic-temporal,mcp-server,simulation --all-targets -- -D warnings`.
- **In-memory cache, no durable format:** half-life stats are recomputable, so
  `CohortStatsCache` is never persisted — no sidecar, no on-disk format change.
- **#3238 cold-tier caveat:** overall statistics reflect available history (hot +
  restored); cold-migrated versions follow the #3238 coverage-caveat model, and
  the response discloses the window it saw.
- **Stage B (green implementation) is pending:** all estimator/scan bodies are
  `todo!()`; this stage is design + compiling skeleton + red tests only.

---

## 13. Red phase evidence

Pure-estimator tests run against the `todo!()` skeleton — all fail, none ignored,
none passed (asserting the tests are genuinely red, not vacuously green):

```
$ cargo test --features semantic-temporal --lib half_life

test result: FAILED. 0 passed; 8 failed; 0 ignored; 0 measured; 4396 filtered out

failures:
    experimental::temporal::half_life::tests::all_censored_has_no_median
    experimental::temporal::half_life::tests::below_floor_is_insufficient_data
    experimental::temporal::half_life::tests::empty_cohort_is_insufficient_data
    experimental::temporal::half_life::tests::freshness_survival_probability_reads_curve
    experimental::temporal::half_life::tests::km_recovers_planted_half_life_within_tolerance
    experimental::temporal::half_life::tests::median_and_percentiles_over_explicit_curve
    experimental::temporal::half_life::tests::single_event_curve_drops_to_zero
    experimental::temporal::half_life::tests::tied_event_times_fold_into_one_step
```

Each panics with `not yet implemented: Stage B: …` from a `todo!()` body (e.g.
`KmCurve::median`, `kaplan_meier`). The e2e tests in `tests/half_life_e2e.rs`
(gated `all(semantic-temporal, simulation)`) likewise fail on the `todo!()`
`AletheiaDB` accessors — Stage B turns them green.
