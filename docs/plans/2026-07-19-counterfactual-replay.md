# Counterfactual Exclusion Replay — Design (Issue #3357)

Status: Draft / in progress. Cohort: `semantic-temporal` (experimental).
Module: `src/experimental/temporal/counterfactual.rs`.

> "What would we believe today if that source had never written?" This design
> materializes a read-only *shadow* view of the database as it would exist had a
> named source's writes never been recorded, so incident response, trust
> evaluation, and data-value attribution become single queries.

---

## 1. Problem & the 8 Acceptance Criteria

When a source turns out to be compromised (a poisoned feed, a buggy pipeline, a
hallucinating agent), operators need to know *"the world without source X"*.
AletheiaDB already records everything needed to answer it: a totally ordered
transaction history (WAL + bi-temporal versions) and per-version source
attribution (#3224). Counterfactual replay closes the loop.

The eight ACs, restated concisely:

1. **Create by predicate.** A caller creates a counterfactual view from an
   exclusion predicate over provenance — at minimum `source == S` and
   `source ∈ {S1..Sn}`, optionally bounded to a transaction-time range. The
   operation returns a handle naming the view.
2. **Exclusion-replay semantics (defined + reported).** Recorded history is
   replayed in transaction-time order with matching writes omitted; a version
   that superseded an excluded version is re-evaluated against the resulting
   timeline. The contract for an *orphaned update* (source X created a node,
   source Y later updated it, X excluded) is explicitly specified, deterministic,
   and reported — never left undefined (see §5).
3. **Fully queryable read-only.** Current-state reads, traversal, `AS OF`
   temporal queries, and history reads against the view behave as if the excluded
   writes never happened, including bi-temporal coordinates — through the existing
   read surfaces (Rust API now; MCP deferred, see §9).
4. **Real DB never mutated.** Creating, querying, and dropping a view leaves live
   state, history, WAL, and all indexes byte-identical (checksum-verified in
   tests). Views are disposable; storage is reclaimed on drop.
5. **Divergence report.** Materialization produces counts of excluded writes,
   entities whose current state differs, and entities removed entirely — "blast
   radius of source X" in one call.
6. **Determinism.** Replaying the same history with the same predicate twice
   yields identical views (identical query answers), verified by a property test.
7. **Unattributed writes never excluded.** Writes recorded without provenance
   match no source predicate and are never excluded; documented as the
   unattributed-data caveat; the report counts unattributed writes encountered.
8. **Guardrails + labeling.** Materialization is bounded by a configurable
   history-size cap with a structured over-limit error; every view response is
   clearly labeled counterfactual so no caller mistakes counterfactual answers
   for real ones.

Out of scope (from the issue): committing a view back into the real DB;
hypothetical *additions*; automatic propagation through derivation lineage;
cross-shard coordination; retention/GC changes.

---

## 2. Brainstorming — how to materialize "the world without source X"

Idea generation (no filtering yet):

- **Filtered WAL re-execution.** Read the WAL from LSN 0, drop excluded ops,
  re-append survivors into a fresh engine. Simple mental model, but re-append
  re-stamps transaction time → loses original coordinates (see §5 risk).
- **Filtered version-record restore.** Enumerate every recorded `NodeVersion` /
  `EdgeVersion` from `HistoricalStorage`, drop excluded ones, insert survivors
  *with their original bi-temporal intervals* into fresh shadow storage via the
  trusted-restore path. Preserves coordinates exactly. (This becomes Approach A.)
- **Read-time virtual overlay.** Never materialize; each read walks the real
  version chain and skips excluded versions on the fly (like `hindsight`, but the
  filter is a provenance predicate over history rather than an entity-id patch).
- **Diff-from-real.** Materialize only the *delta* — entities whose chains touch
  an excluded write — and fall through to real storage for everything else.
- **Copy-on-exclude snapshot.** Clone the whole hot tier, then surgically rewind
  every entity that has an excluded version in its chain.
- **Provenance-indexed reverse map.** Pre-index `source → [versions]`; excluding a
  source is then a set lookup, and only touched entities need recomputation.
- **Query-rewrite.** Push the exclusion into the query planner as an implicit
  `WHERE provenance.source != S` on every version scan. (Rejected: doesn't
  re-derive orphaned-update fallout; only hides what you see, like Datomic.)
- **Event-sourced fold.** Treat history as an event log and fold it minus excluded
  events into a fresh projection — the functional framing of Approach A.

---

## 3. Reverse brainstorming — how could we get this catastrophically wrong?

Each failure is mapped to the AC that must guard against it.

| # | Catastrophic failure | Guarded by |
|---|---|---|
| R1 | **Mutate the real DB** while building/dropping the view (share storage, write through a handle). | AC4 — separate shadow storage; checksum test before/after. |
| R2 | **Non-deterministic replay** (iterate a `HashMap`, tie-break on wallclock only, thread races) → two runs disagree. | AC6 — replay over the `ChangeCursor` *total* order, pure fold, no map-order leakage; property test. |
| R3 | **Exclude unattributed writes** (treat `None` provenance as "not source S" *inclusively*, or as a wildcard). | AC7 — predicate `matches(None) == false` always; count them separately. |
| R4 | **Unbounded memory** — materialize a billion-version history into RAM and OOM. | AC8 — `max_replay_versions` cap + structured `HistoryTooLarge` error *before* allocation. |
| R5 | **Orphaned-update ambiguity** — silently drop, silently promote-to-create, or panic when source Y's update targets an entity whose creating write was excluded. | AC2 — explicit deterministic drop-and-count contract (§5); reported. |
| R6 | **Counterfactual answers mistaken for real** — a view read looks byte-identical to a real read; an operator acts on fiction. | AC8 — every envelope labeled `is_counterfactual: true` + view name in reports. |
| R7 | **Lost bi-temporal coordinates** — replay re-stamps transaction time, so `AS OF` reads land on the wrong version. | AC3 — restore versions with their *original* `BiTemporalInterval` (§5), not via WAL re-append. |
| R8 | **Excluded data leaks back in** — a surviving update's carried-forward properties re-introduce the excluded source's fields. | AC2/AC7 — drop-and-count orphaned updates rather than promoting them (§5, the corrected rationale). |
| R9 | **Storage not reclaimed on drop** — the view leaks the shadow copy. | AC4 — `CounterfactualView` owns its shadow storage; `Drop` frees it. |
| R10 | **Divergence miscount** — double-count re-created entities, or miss deletes. | AC5 — key the report by `EntityId`, compare current-state via `FactStatus`/`VersionDiff`. |

---

## 4. Six Thinking Hats

- **White (facts / what we have).** `#3224` write-time provenance persisted on
  every `VersionInfo` (`Option<Provenance>`) and on every `WalOperation` (all
  variants carry `Option<Provenance>`). `ChangeCursor` gives a total transaction-
  time order `(tx_wallclock, tx_logical, kind_ord, entity_id, version_id)`.
  `HistoricalStorage::insert_restored_node_version` / `insert_restored_edge_version`
  are `pub(crate)` trusted-restore entry points that insert a version verbatim,
  preserving its exact `BiTemporalInterval`. `ProvenanceFilter` (#3348) already
  implements any-of source matching with `matches(None) == false`. `VersionInfo`
  and `WalOperation::UpdateNode` both carry **full** merged properties, not deltas.
  The `belief_revision` module is the structural sibling (same cohort, same
  read-only-over-history shape).
- **Red (intuition / risk feel).** The scary part is *fidelity*: an operator will
  make an incident-response decision on this output. If a single excluded field
  leaks back through a surviving update, or `AS OF` returns a subtly wrong
  version, the feature is worse than useless — it is misleading. Coordinate
  preservation and orphan handling deserve the most paranoia.
- **Black (dangers).** Memory blow-up on large histories (R4). Determinism traps
  in Rust `HashMap` iteration (R2). The temptation to reuse WAL re-append, which
  quietly destroys transaction-time coordinates (R7). Confusing counterfactual
  output for real (R6).
- **Yellow (benefits).** A category-defining capability no incumbent can express.
  Reuses existing, battle-tested read paths (historical reconstruction, `AS OF`)
  by binding them to shadow storage — high correctness leverage for low new code.
  The report answers "blast radius" in one call.
- **Green (creative alternatives).** Lazy overlay (Approach B) avoids upfront
  cost; hybrid compact materialization (Approach C) bounds memory to touched
  entities; a `source → versions` reverse index could make re-materialization
  incremental. All are follow-ups; A is the correct first cut.
- **Blue (process / plan).** (1) This design doc + gated compiling scaffold +
  draft PR (anti-loss). (2) Red-phase tests from the §8 matrix. (3) Implement the
  filtered-restore replay + divergence report. (4) Wire read delegation to shadow
  storage. (5) Property test for determinism, checksum test for immutability.
  (6) Bench materialization on the 10K/50K reference history (<30s target).
  (7) MCP surface as a separate follow-up (deferred, §9).

---

## 5. Implementation approaches

### Approach A — Materialized shadow storage (CHOSEN)

On view creation:

1. **Enumerate** every recorded version from the real `HistoricalStorage` (node
   and edge version records).
2. **Order** them by the `ChangeCursor` total order
   `(tx_wallclock, tx_logical, kind_ord, entity_id, version_id)` — the
   transaction-time replay order AC2 requires.
3. **Filter**: skip any version whose recorded `provenance` matches the exclusion
   predicate (source ∈ set) *and* falls inside the optional tx-time bound. Count
   excluded and unattributed-encountered as we go.
4. **Apply survivors** into a **fresh in-memory** shadow `HistoricalStorage`
   (+ derived `CurrentStorage` heads) via the trusted-restore bypass so each
   surviving version keeps its **exact** `BiTemporalInterval`.
5. **Reads delegate** to the existing historical read implementations bound to the
   shadow storage (`get_node_at_time`, `get_node_history`, reconstruction,
   traversal), giving AC3 fidelity by reuse.

- **Pros:** read fidelity by reuse (AC3); real DB trivially untouched via
  separate storage (AC4); strong determinism — a pure fold over a totally-ordered
  stream (AC6); bi-temporal coordinates preserved exactly (AC3/R7).
- **Cons:** memory equals a second copy of the *surviving* history (bounded by the
  AC8 cap); one-time materialization cost.

### Approach B — Lazy virtual overlay

Filter version chains at read time (the `hindsight` overlay pattern, but over
history + provenance): every read walks the real chain and skips excluded
versions on the fly.

- **Pro:** no upfront materialization cost; zero shadow memory.
- **Con:** every read path (current, `AS OF`, history, traversal) must be
  reimplemented view-aware — large surface, easy to diverge from the real
  semantics; latency guarantees are harder; the divergence report still needs a
  full history scan, so the "no upfront cost" advantage is partly illusory when
  AC5 is mandatory.

### Approach C — Hybrid compact materialization

Materialize only entities whose chains touch an excluded write; fall through to
real storage for untouched entities.

- **Pro:** bounded memory (only affected entities copied).
- **Con:** correctness of the fall-through boundary is subtle — an untouched
  entity read must be provably identical to the real read, and traversal crossing
  the boundary must compose the two stores without artifacts. Noted as a **future
  optimization** once A is correct and benchmarked.

### Why A

AC3 ("through the existing read surfaces") and AC4 ("byte-identical real DB") are
the two hardest, highest-stakes ACs, and A satisfies both *by construction*:
reads reuse the real reconstruction code against a physically separate store, so
correctness is inherited rather than re-derived, and immutability is structural
rather than promised. Determinism (AC6) is a pure fold over a total order. The
only real cost — memory — is exactly what AC8's cap exists to bound. B and C
trade that structural safety for performance we do not yet need at the reference
scale (<30s over 10K/50K), so they are deferred optimizations.

---

## 6. Orphaned-update contract (AC2)

**Decision: excluded-as-unappliable (drop-and-count).**

When an excluded write is skipped during replay, a *later* write by another source
may target an entity that now has **no surviving prior version** at that point in
the replay. Contract:

- An update/delete targeting an entity that **still has a surviving prior
  version** applies normally on top of the surviving chain.
- An update/delete targeting an entity with **no surviving prior version** is
  *unappliable*: it is **dropped** from the view and counted as `orphaned_updates`
  in the divergence report.
- An entity with **no surviving version at all** (its whole chain was excluded) is
  **removed entirely** and counted as `entities_removed`.

This is **deterministic** (a function of the total-ordered stream and the
predicate) and **reported** (dedicated report counters), satisfying AC2's
"explicitly specified, deterministic, and reported".

### Code verification (performed against this tree) and how it changed the wording

Findings from `src/core/history.rs`, `src/storage/historical/mod.rs`,
`src/storage/wal/entry.rs`:

1. **Versions carry FULL state, not deltas (at the API level).** `VersionInfo.properties`
   is the *reconstructed* full `PropertyMap` at that version, and
   `WalOperation::UpdateNode { properties }` is documented as "The new properties"
   — the full post-merge map (the write path merges patch → full state *before*
   the WAL append / version store). Anchor+delta compression is an *internal*
   storage encoding; the surfaced version is full state.
2. **History distinguishes op kinds.** Create = `version_number == 1`; delete /
   #3230 retraction = **closed** valid interval (`temporal.valid_time().is_closed()`,
   exactly the discriminator `belief_revision` uses); update = `version_number > 1`
   with an open interval.

**This changed the contract's rationale (recorded honestly here).** The task's
draft rationale was "a patch cannot reconstruct full state, so promoting an
orphaned update to a create is unsound." That rationale is **false in this
engine** — a surviving update *does* carry full state, so we technically *could*
promote it to a create. The **actual** reason drop-and-count is the sound contract
is the inverse: a surviving update's full-state snapshot **carries forward the
excluded source's untouched properties** (fields the excluded create set that the
later update never modified are still present in the update's full map). Promoting
that update to a create would **silently re-introduce the excluded source's data**
through the back door — violating the exclusion contract (AC2) and the AC7 "never
resurrect excluded provenance" spirit. Dropping-and-counting is the only option
that *guarantees no excluded write leaks into the view*. So the decision stands,
but the justification is "exclusion soundness / no leak-back" (R8), not
"reconstruction impossibility".

### Reconstruction mechanism (the #1 implementation risk — nailed down)

**Mechanism chosen: direct trusted-restore insertion, NOT WAL re-append.**

Survivors are applied into the fresh shadow `HistoricalStorage` by constructing
each surviving version's `NodeVersion` / `EdgeVersion` record (carrying its
**original** `BiTemporalInterval`) and inserting it via the `pub(crate)`
`HistoricalStorage::insert_restored_node_version` /
`insert_restored_edge_version` bypass — the *same* trusted-source path that
index-persistence restore uses (`src/storage/historical/mod.rs:3718 / :3751`). The
counterfactual module lives inside the crate, so these `pub(crate)` entry points
are callable.

**Why not synthesize a `WalOperation` stream through
`replay_entries_into_storage_with_constraints` (`src/storage/recovery.rs:170`)?**
That path re-executes ops and **re-stamps transaction time at WAL append / apply**
(the recovery resolver pairs each op with a *commit* timestamp; a fresh append
assigns a fresh LSN/timestamp). It would therefore **destroy** the original
transaction-time coordinates, breaking AC3's "including bi-temporal coordinates"
and `AS OF SYSTEM_TIME` reads (R7). The trusted-restore path preserves the exact
`(valid, transaction)` interval of every surviving version, which is precisely
what AC3 demands.

v1 detail: survivors may be materialized as **anchors** carrying reconstructed
full properties (correctness first; the shadow store re-derives its own
anchor+delta compression as an optimization later). The current-state
`CurrentStorage` heads are derived from each entity's surviving open-interval head
version.

---

## 7. Public API (Rust)

Gated `#[cfg(feature = "semantic-temporal")]`, in
`src/experimental/temporal/counterfactual.rs`.

```rust
// Exclusion predicate over provenance (+ optional tx-time bound).
pub struct ExclusionPredicate { /* wraps ProvenanceFilter + tx bounds */ }
impl ExclusionPredicate {
    pub fn source(source: impl Into<String>) -> Self;
    pub fn sources(sources: impl IntoIterator<Item = String>) -> Self;
    pub fn within_transaction_time(self, from: Option<Timestamp>, to: Option<Timestamp>) -> Self;
    // internal: true == "exclude this write". Unattributed (None) => false (AC7).
    fn excludes(&self, provenance: Option<&Provenance>, tx_time: Timestamp) -> bool;
}

pub struct CounterfactualConfig { pub max_replay_versions: usize } // + Default

pub struct DivergenceReport { /* counts + changed/removed entity ids */ }
// accessors: excluded_writes(), unattributed_writes_encountered(),
// orphaned_updates(), entities_changed(), entities_removed(),
// changed_entities(), removed_entities(); Serialize (serde-gated).

pub struct CounterfactualHandle { /* name */ }

pub struct CounterfactualView { /* shadow storage + report + handle */ }
impl CounterfactualView {
    pub fn report(&self) -> &DivergenceReport;
    pub fn handle(&self) -> &CounterfactualHandle;
    pub fn is_counterfactual(&self) -> bool; // always true (AC8)
    // read surface (shapes mirror the real API), bound to shadow storage:
    pub fn get_node(&self, id: NodeId) -> Result<Node, CounterfactualError>;
    pub fn get_edge(&self, id: EdgeId) -> Result<Edge, CounterfactualError>;
    pub fn get_node_at_time(&self, id: NodeId, valid: Timestamp, tx: Timestamp)
        -> Result<Node, CounterfactualError>;
    pub fn get_node_history(&self, id: NodeId) -> Result<EntityHistory, CounterfactualError>;
}

pub enum CounterfactualError {
    Unimplemented,
    HistoryTooLarge { versions: usize, cap: usize },
    NotFound(String),
    Internal(String),
}

impl AletheiaDB {
    pub fn counterfactual_replay(
        &self,
        name: impl Into<String>,
        predicate: ExclusionPredicate,
        config: CounterfactualConfig,
    ) -> Result<CounterfactualView, CounterfactualError>;
}
```

Names may be refined during implementation to match repo conventions observed in
`belief_revision` / `snapshot`.

---

## 8. Guardrails & labeling (AC8)

- **Cap.** `CounterfactualConfig::max_replay_versions`, **default `5_000_000`**.
  Justification: the config precedent is `max_schema_as_of_entities` (default
  `50_000`), but that bounds a point-in-time *entity* scan, whereas replay
  materializes *whole-history version records* — a categorically larger working
  set. The 10K-node / 50K-edge reference history has on the order of ≤ 10⁶ total
  versions; `5_000_000` gives generous headroom while still bounding shadow memory
  to a second copy of surviving history and providing a hard OOM backstop. It is
  configurable for operators with larger histories.
- **Structured over-limit error.** When the enumerated version count exceeds the
  cap, return `CounterfactualError::HistoryTooLarge { versions, cap }` **before**
  allocating the shadow store (fail fast, R4). This maps to MCP
  `FAILED_PRECONDITION` (non-retriable) when the MCP surface lands.
- **Labeling.** `CounterfactualView::is_counterfactual()` is *always* `true`, the
  view's `name` surfaces in the handle and in every divergence report, and (when
  MCP lands) every response envelope carries a `counterfactual: true` + view-name
  marker so no caller can mistake a counterfactual answer for real state (R6).

---

## 9. MCP surface: DEFERRED

The MCP `counterfactual_replay` tool and view read-routing are **deferred to a
follow-up** (a registry-slot constraint means another lane owns the MCP
registration in this wave). AC3's MCP clause is satisfied **at the substrate
level** by the Rust read surface now (the shadow-bound reads); the MCP wrappers
land later. The follow-up will mirror the **belief_revision MCP pattern** exactly:
a `#[cfg(feature = "semantic-temporal")]` handler + a
`#[cfg(not(feature = "semantic-temporal"))]` twin returning a structured
`FAILED_PRECONDITION` with `{tool, required_feature: "semantic-temporal"}`,
registered `AccessClass::Read` in `src/mcp/auth.rs` (creating a view is a read —
AC4 says the real DB is never mutated), threaded through the #3234 structured
error codes and #3353 token-budget wrappers.

---

## 10. Risks / edge cases — test matrix (red-phase tests)

| Risk | Test name |
|---|---|
| Exclude a single source | `test_exclude_single_source_removes_its_writes` |
| Exclude a set of sources | `test_exclude_source_set_any_of` |
| Tx-time-range bound on the predicate | `test_exclude_bounded_by_transaction_time_range` |
| Unattributed never excluded + counted (AC7) | `test_unattributed_writes_never_excluded_and_counted` |
| Orphaned update dropped + counted (AC2) | `test_orphaned_update_dropped_and_counted` |
| Entity with whole chain excluded removed entirely | `test_entity_removed_entirely_when_all_versions_excluded` |
| Real DB byte-identical checksum before/after create+query+drop (AC4) | `test_real_db_checksum_unchanged_across_view_lifecycle` |
| Determinism property test (AC6) | `prop_replay_is_deterministic` |
| `AS OF` / history reads against the view (AC3) | `test_view_as_of_and_history_reads_reflect_exclusion` |
| Divergence report counts (AC5) | `test_divergence_report_counts_changed_and_removed` |
| Over-cap error (AC8) | `test_history_too_large_returns_structured_error` |
| Counterfactual labeling present (AC8) | `test_view_is_labeled_counterfactual` |
| Empty predicate / no matches ⇒ no-op view equals real | `test_no_match_predicate_yields_view_equal_to_real` |

---

## 11. References

- Issue #3357 (this spec); #3224 write-time provenance (shipped substrate);
  #3348 `ProvenanceFilter`; #3362 belief-revision audit (structural sibling,
  same cohort); ADR-0038 `hindsight` (prior-art counterfactual overlay — different
  axis); ADR-0050 experimental feature categorization.
- Code: `src/core/provenance.rs` (`ProvenanceFilter`), `src/core/history.rs`
  (`VersionInfo`/`EntityHistory`/`VersionDiff`), `src/core/changefeed.rs`
  (`ChangeCursor` total order), `src/storage/historical/mod.rs`
  (`insert_restored_node_version` :3718 / `insert_restored_edge_version` :3751),
  `src/storage/recovery.rs` (`replay_entries_into_storage_with_constraints` :170 —
  *not* used, see §6), `src/experimental/temporal/belief_revision.rs` (pattern).
