# Plan: Temporal Aggregation Windows over Entity History in AQL (Issue #3363)

Complexity tier: **L**. Lane: `src/query/**` (+ `src/cypher`, `src/sql` owned but
untouched — Cypher aggregation is explicitly out of scope per the issue).
Constraint: **must not** edit `src/mcp/**` or `crates/aletheia-server`.

## 1. Problem restatement

AletheiaDB can reconstruct any entity at any bi-temporal coordinate but cannot
*summarize across time*. This adds a one-statement AQL construct that buckets a
matched entity's (or matched set's) **valid-time** history into fixed
(tumbling) windows and computes per-window aggregates, replacing O(windows)
client-side `AS OF` round-trips.

## 2. Acceptance criteria (verbatim from #3363)

1. AQL grammar supports a windowed temporal aggregation construct over a
   valid-time range — match pattern → declare window granularity (`1 hour`,
   `1 day`, `1 month`) over an explicit `[start, end)` valid-time range →
   `RETURN` per-window aggregates. Keyword syntax is Engineering's choice;
   documented in the AQL grammar reference.
2. Supported aggregates v1: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX` over numeric
   properties, plus `CHANGES` (number of distinct versions of the matched
   entity/property whose valid intervals start within the window).
3. Window semantics documented and falsifiable: value attributed to a window =
   value as of a documented sampling rule (at minimum: value at window start).
   Fixture with hand-computed values per window passes exactly.
4. Interval-edge correctness: a version whose valid interval starts exactly at
   a window boundary is counted in exactly one window (boundary rule
   documented); open-ended intervals handled through the final window; windows
   with no valid data return an explicit empty/`null` row, not dropped.
5. Transaction-time dimension respected: results reflect history as recorded at
   a caller-supplied `AS OF SYSTEM_TIME` (default now).
6. Works through the MCP `query` tool (read-only path, existing error
   contract): a malformed window spec returns a structured
   `parse_error`/`invalid_params`, never a panic or empty success.
7. Result rows include window start/end (RFC 3339) alongside the aggregates.
8. Documentation includes ≥2 worked examples (monthly `AVG` of a price; weekly
   `CHANGES` volatility) with expected output.

Success metric: 12-window monthly aggregation over ≤1,000 versions < 50ms
(benchmark). Window-edge fixture passes 100% vs hand-computed values.

## 3. Chosen grammar

```
MATCH (v:Label {prop: value})
WINDOW <n> <unit> OVER VALID_TIME FROM <ts> TO <ts>
[ AS OF SYSTEM_TIME <ts> ]
RETURN <agg>(v.prop | *) [AS alias] [, ...]
```

- `<unit>` ∈ `MINUTE(S) | HOUR(S) | DAY(S) | WEEK(S) | MONTH(S) | YEAR(S)`
  (also `QUARTER(S)` = 3 months). MINUTE/HOUR/DAY/WEEK are fixed-duration;
  MONTH/QUARTER/YEAR are calendar-based (chrono month arithmetic, UTC).
- `<ts>` = RFC 3339/ISO-8601 string, or integer/string microseconds since
  epoch (superset of existing AQL timestamp handling; existing AQL only did
  micros, so this is strictly additive).
- The WINDOW clause sits after the source (MATCH) and before RETURN. The
  tx-time coordinate lives *inside* the window clause (`AS OF SYSTEM_TIME`) to
  avoid ambiguity with the pre-MATCH `AS OF <valid>[, <tx>]` temporal clause.

### Sampling & boundary rules (documented, falsifiable)

- **Boundary rule:** windows are half-open `[b_k, b_{k+1})`, `b_0 = start`,
  `b_{k+1} = step(b_k)`, generated until `b_k >= end`; the final window's end is
  **clamped to `end`**. Union of windows == exactly `[start, end)`, no
  overlap/gap. A version whose `valid_from` equals a boundary belongs to the
  window it *starts* (`b_k <= valid_from < b_{k+1}`).
- **Value sampling (SUM/AVG/MIN/MAX/COUNT):** value attributed to window `k` =
  the entity's property value **as of window start** `(valid = b_k,
  tx = as_of_system_time)`, reconstructed via
  `find_node_version_at_time` + `reconstruct_node_properties`. One sample per
  matched entity per window.
  - `SUM/AVG/MIN/MAX(v.prop)` aggregate the numeric samples across the matched
    set (non-numeric/absent samples skipped, mirroring #558 leniency).
  - `COUNT(v.prop)` = number of matched entities with a defined numeric sample
    at window start. `COUNT(*)` = number of matched entities that exist (any
    version valid) at window start.
- **CHANGES:** `CHANGES(v)`/`CHANGES(*)` = number of entity versions (across the
  matched set) whose `valid_from ∈ [b_k, b_{k+1})`, counting only versions
  believed as-of `as_of_system_time` (transaction interval contains the
  tx coordinate). `CHANGES(v.prop)` = of those in-window versions, the count
  whose value for `prop` differs from the immediately-preceding (by valid
  order) believed version's value (genuine property change).
- **Empty window:** row always emitted. `COUNT`/`CHANGES` → `0` (Int);
  `SUM/AVG/MIN/MAX` → `Null`.

## 4. Implementation approaches considered

- **A. New terminal `QueryOp` + executor iterator (CHOSEN).** Lower
  `MATCH…WINDOW…RETURN` into `[ScanNodes/Filter…, TemporalWindowAggregate(spec)]`.
  Mirror the existing `Aggregate` op through `plan.rs` (`UnaryOp`),
  `planner/{mod,physical,cost}.rs`, `executor/mod.rs` dispatch, and a new
  `TemporalWindowAggregateIterator`. Pure window math factored into
  `src/query/temporal_window.rs` for unit testing.
  *Cost:* ~8 files, mostly mechanical one-arm additions. *Wins:* stays entirely
  in `src/query`; `execute_aql`, the MCP `query` tool, EXPLAIN/PROFILE all keep
  working with zero edits outside the lane; reuses MATCH source + property
  filters; errors already map (SyntaxError→`parse_error`,
  InvalidParameter→`invalid_params`, UnsupportedFeature→`unsupported_construct`).
- **B. Dedicated evaluator routed from `execute_aql`** (like Cypher
  MultiPattern/Mutation). *Cost:* smaller query-internal surface but requires
  editing `src/db/query.rs::execute_aql`, outside the owned lane, and would
  bypass planner EXPLAIN/PROFILE.
- **C. Overload existing `Aggregate` op with a window field.** *Cost:* pollutes
  the streaming single-coordinate aggregator with a fundamentally different
  history-reading execution model; rejected as muddying a hot path.

**Pick: A.** Idiomatic (every AQL/Cypher feature lowers to `QueryOp`), fully
in-lane, no MCP/server/db edits, structured errors for free.

## 5. Reverse brainstorm — how could this silently produce wrong results?

Each becomes a test:
- Off-by-one at window boundary (version at `valid_from == b_k` double-counted
  or dropped). → boundary fixture.
- Last window extends past `end`, double-counting versions in `[end, b_last)`.
  → clamp test.
- Calendar month treated as 30 days → Feb/31-day drift. → monthly fixture with
  hand-computed month starts.
- AS OF SYSTEM_TIME ignored → later corrections rewrite past analytics. →
  bi-temporal test: correct a value, re-run with old tx time, expect old result.
- Empty window silently dropped → caller sees 11 rows for 12 months. → empty
  window test asserts row present with null/0.
- Open-ended (still-valid) version not sampled in windows after its start. →
  open-interval test.
- Overflow of i64 sum → wrap. → reuse #558 i128/float-promote accumulator.
- Non-numeric property fed to SUM → panic or garbage. → skip + test.
- Malformed spec (end<=start, zero count, unknown unit, unparseable ts) →
  panic/empty success. → structured-error tests.
- RFC3339 vs micros parsing divergence between FROM and TO. → parity test.

## 6. Six-hats (condensed)

- White (facts): history APIs `find_node_version_at_time`,
  `reconstruct_node_properties`, `get_node_history` exist; chrono is a
  non-optional dep; column rows already surface through MCP (server.rs:5719).
- Red (gut): the correctness risk is all in interval math — isolate & fixture it.
- Black (caution): don't perturb the streaming aggregator or planner cost of
  existing queries; new op only.
- Yellow (benefit): one query replaces ≥12 round-trips; falsifiable fixture.
- Green (creative): factor pure boundary/step math into its own module so it is
  testable without a DB.
- Blue (process): TDD — parser tests, boundary unit tests, then end-to-end
  bi-temporal fixtures, then bench.

## 7. Risks / edge cases as concrete test cases

1. `window_boundary_start_counted_once` — version at exact boundary in one bucket.
2. `last_window_clamped_to_end` — no double count past `end`.
3. `monthly_calendar_windows_hand_computed` — 12 monthly AVG rows exact.
4. `weekly_changes_volatility_hand_computed` — CHANGES per week exact.
5. `as_of_system_time_respects_correction` — old tx time yields pre-correction.
6. `empty_window_emits_null_row` — 0/null, not dropped.
7. `open_ended_interval_sampled_through_final_window`.
8. `sum_overflow_promotes_to_float`.
9. `non_numeric_property_skipped`.
10. `malformed_spec_structured_error` (end<=start, count 0, unknown unit).
11. `rfc3339_and_micros_boundaries_parity`.
12. `aql_equiv_micros_vs_rfc3339` (AQL≡AQL parity for two timestamp encodings).
13. AQL≡Cypher parity note: Cypher temporal windows are **out of scope** (#558
    follow-up); parity here is between the two AQL timestamp encodings and
    between value-sampling done two ways. Documented explicitly.
14. `multi_entity_avg_across_set` — matched set averaging.
15. Bench: `bench_monthly_window_1000_versions < 50ms`.

## 8. Deliverables

- Grammar: lexer tokens (`WINDOW/OVER/VALID_TIME/SYSTEM_TIME/FROM/CHANGES` +
  unit words as contextual identifiers), parser clause, AST `WindowClause`.
- IR: `QueryOp::TemporalWindowAggregate(TemporalWindowSpec)` + supporting types.
- Converter: lower MATCH+WINDOW+RETURN, with semantic validation → structured
  errors.
- Pure math module `src/query/temporal_window.rs` (boundary gen, calendar step,
  ts parse/format) with unit tests.
- Executor `TemporalWindowAggregateIterator`.
- Planner/physical/cost plumbing.
- Docs: AQL grammar reference section + 2 worked examples.
- Benchmark.

## 9. v1 limitations (documented)

- Windowed variable must bind a **node** (v1). Edge windows return a structured
  `UnsupportedFeature` error (documented follow-up) unless cheap to add.
- Matched candidate set is resolved from the pattern at current state (entity
  must currently exist for its history to be windowed); windowing history of
  since-deleted entities is a follow-up.
- Tumbling (fixed, non-overlapping) windows only — no sliding/hopping/session
  (explicitly out of scope in #3363).
