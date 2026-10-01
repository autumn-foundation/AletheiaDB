# Contradiction Genealogy (Issue #3352)

**Status:** Draft / in implementation. **Feature flag:** `semantic-temporal` (experimental cohort, ADR-0050). **Complexity:** L. Read-only analysis engine over bi-temporal + provenance history; no storage-format/WAL change.

## 1. Problem & goal

When an agent finds two facts that disagree ("Acme's CEO is Alice" vs "Bob"), AletheiaDB uniquely holds what is needed to adjudicate: valid time (when each was true), transaction time (when we learned it), provenance (source + confidence), and graph structure. Contradiction genealogy turns silent knowledge-base rot into an inspectable artifact: given a conflict target, reconstruct every competing claim's bi-temporal life, attribute each to its sources, locate the divergence point, and classify retroactive corrections vs contemporaneous disagreement — read-only and deterministic.

## 2. Bi-temporal ground truth (empirically verified)

AletheiaDB strictly **linearizes versions on transaction time**: each write closes the prior head version's `transaction_to` at the successor's `transaction_from`, so at any single transaction coordinate **exactly one** version is tx-current (`src/storage/historical/mod.rs:3372-3404`). Crucially, a superseded version's **valid-time interval is left open and unsplit** (Issue #3504, same location): updating `ceo=Alice valid[t0,∞)` to `ceo=Bob valid[t1,∞)` yields two versions whose **valid intervals overlap** on `[max(t0,t1),∞)` with **differing values**, while their transaction intervals are disjoint.

**This overlap is the structural contradiction.** The record literally asserts both "Alice from t0" and "Bob from t1" over the overlap, because the Alice claim was never retracted. The escape hatch already exists: **retraction** (Issue #3230 `retract_node`/`retract_edge`) closes the prior claim's `valid_to`, removing the overlap — a clean succession then produces **no** contradiction. So the feature precisely flags value changes made *without* retracting the superseded claim: silent knowledge-base rot.

A consequence (verified): point-in-time reads at `tx=now` can gap on earlier valid-time windows shadowed by a backdated write; genealogy therefore reads the **append-only history** (`get_node_history`/`get_edge_history`), not point-in-time reconstruction.

## 3. v1 contradiction definition (falsifiable, AC2)

For a single entity+property, a **contradiction** exists iff there are ≥ 2 versions (claims) whose:
- **property key** is the same, and
- **asserted values differ** (`PropertyMap::get(k)` inequality; `PropertyValue` is `PartialEq`, not `Eq` — Float/Vector NaN caveat documented), and
- **valid-time intervals overlap** (`[max(vf),min(vt))` non-empty; open end = `TIMESTAMP_MAX`).

Because the engine linearizes tx-time, the two claims are never *simultaneously tx-current*; the overlap lives across transaction time in the append-only history. A retracted prior claim (closed `valid_to` not overlapping the successor's `valid_from`) is **not** a contradiction. This is exact and seed-testable: 100% recall / 100% precision against a fixture built to this definition.

## 4. Classification — temporal honesty (AC3)

Each conflicting pair (earlier-recorded A, later-recorded B, `txA < txB`) is labeled:
- **RetroactiveCorrection** — `B.valid_from ≤ A.valid_from` (B reached back to (re)assert over a window A already covered from its start; a later transaction rewrote the past by backdating). Mirrors belief-revision `RevisionClass::Correction` (Issue #3709, `belief_revision.rs`): `valid_from ≤ running-max prior`.
- **ContemporaneousDisagreement** — `B.valid_from > A.valid_from` (B extends forward but, because A was never retracted, both remain asserted over the overlap; two claims left standing). Mirrors `RevisionClass::WorldChange`.

Per-claim `origin` reuses `RevisionClass` (InitialAssertion/Correction/WorldChange/Retraction/Reaffirmation) computed over the entity's version sequence, so the genealogy is consistent with the belief-revision audit (#3362/#3709).

## 5. Divergence point (AC1)

For a conflicting pair, the **divergence point** is the earliest bi-temporal coordinate at which the record first contained both claims over an overlapping valid window:
`transaction_time = B.transaction_from` (when the second claim entered), `valid_time = max(A.valid_from, B.valid_from)` (start of the valid overlap). The genealogy's top-level `divergence_point` is the earliest such coordinate across all conflicting pairs (min transaction_time, tie-break earliest valid). Deterministic.

## 6. Public API (Rust)

Module `src/experimental/temporal/contradiction_genealogy.rs`, gated `#[cfg(feature = "semantic-temporal")]`, engine `ContradictionGenealogyEngine<'a> { db: &'a AletheiaDB }` plus convenience methods on `AletheiaDB` (mirrors `belief_revisions`). Pure read; no new `AletheiaDB` field.

```rust
impl AletheiaDB {
    pub fn contradiction_genealogy(&self, target: ContradictionTarget, options: &GenealogyOptions) -> Result<ContradictionGenealogy>;
    pub fn find_contradictions(&self, scope: &ContradictionScope) -> Result<ContradictionScan>;
}

pub enum ContradictionTarget {
    EntityProperty { entity: EntityId, property: String },
    Claims(Vec<ClaimRef>),                 // explicit competing-claim set (may span entities)
}
pub struct ClaimRef { pub entity: EntityId, pub version: VersionId }

pub struct GenealogyOptions {              // builder-style .with_*
    pub as_of_transaction_time: Option<Timestamp>,   // time-travel: only claims recorded by then
    pub max_claims: Option<usize>,                   // bound output (elision)
    pub max_sources: Option<usize>,
}

pub struct ContradictionScope {
    pub entity_kind: EntityKindScope,      // Nodes | Edges | Both
    pub label: Option<String>,             // node label / edge type filter
    pub property: Option<String>,
    pub valid_time_window: Option<(Timestamp, Timestamp)>,
    pub transaction_time_window: Option<(Timestamp, Timestamp)>,
    pub limit: usize,                      // default 100, clamped to max 1000 (existing MCP list contract)
    pub offset: usize,
}

pub struct ContradictionScan {
    pub contradictions: Vec<ContradictionSummary>,   // one per (entity, property) group
    pub scanned_entities: usize,
    pub sampled: bool,                     // candidate cap hit (max_schema_as_of_entities)
    pub has_more: bool,
    pub next_offset: Option<usize>,
}
pub struct ContradictionSummary { pub entity: EntityId, pub property: String, pub claim_count: usize,
    pub divergence_point: BiTemporalCoordinate, pub classification: DivergenceKind }

pub struct ContradictionGenealogy {
    pub entity: Option<EntityId>, pub property: Option<String>,
    pub claims: Vec<CompetingClaim>,       // deterministic order (transaction_from, version_id)
    pub pairs: Vec<DivergencePair>,
    pub divergence_point: Option<BiTemporalCoordinate>,
    pub sources: Vec<SourceSummary>,       // AC4
    pub narrative: String,                 // AC5 prose
    pub truncated: bool,                   // AC5 elision disclosure
}
pub struct CompetingClaim { pub claim: ClaimRef, pub value_display: String,
    pub valid_from: Timestamp, pub valid_to: Option<Timestamp>,
    pub transaction_from: Timestamp, pub transaction_to: Option<Timestamp>, pub is_current: bool,
    pub provenance: Option<ClaimProvenance>, pub origin: RevisionClass,
    pub supersedes: Option<VersionId>, pub superseded_by: Option<VersionId> }
pub struct ClaimProvenance { pub source: Option<String>, pub confidence: Option<f64>, pub note: Option<String> }
pub struct DivergencePair { pub earlier: ClaimRef, pub later: ClaimRef, pub kind: DivergenceKind,
    pub coordinate: BiTemporalCoordinate, pub overlapping_valid_from: Timestamp, pub overlapping_valid_to: Option<Timestamp> }
pub enum DivergenceKind { RetroactiveCorrection, ContemporaneousDisagreement }
pub struct SourceSummary { pub source: Option<String>, pub backs_values: Vec<String>, pub claim_count: usize,
    pub min_confidence: Option<f64>, pub max_confidence: Option<f64>, pub latest_confidence: Option<f64>,
    pub most_recent_assertion: Timestamp }
pub struct BiTemporalCoordinate { pub transaction_time: Timestamp, pub valid_time: Timestamp }
```

Errors (#3234): entity/version not found → `NOT_FOUND`; empty `Claims` set or unknown property with no versions → `INVALID_ARGUMENT`; MCP feature-off twin → `FAILED_PRECONDITION` with `required_feature: "semantic-temporal"`. All non-retriable (read-only, deterministic).

## 7. Detection algorithm

Per entity+property (pure fn over `EntityHistory`):
1. Collect versions with the property present; record value, valid interval, tx interval, provenance.
2. For each ordered pair (A earlier-tx, B later-tx): conflict iff `value(A) != value(B)` and valid intervals overlap. Collect conflicting pairs; the set of distinct versions participating are the `claims`.
3. Classify each pair (§4); compute per-pair and top-level divergence point (§5).
4. Aggregate sources (§8). Build narrative. Apply `max_claims`/`max_sources` elision → `truncated`.

`find_contradictions`: enumerate candidate entities (node label index / edge-type index; `entity_kind` scope), capped at `max_schema_as_of_entities` (default 50 000, lowest ids; `sampled` when hit — mirrors #3236/#3360). For each candidate, run detection over each in-scope property; a (entity, property) with ≥1 conflicting pair yields one `ContradictionSummary`. Apply time-window scoping, then `offset`/`limit` (clamped) with `has_more`/`next_offset`. Complexity: O(versions²) per entity (bounded; ≤1K versions per success metric), O(entities) per scan.

## 8. Source attribution (AC4)

Group competing claims by `provenance.source()` (a `None` "unattributed" bucket — never dropped, mirroring #3705 neutral-default). Per source: distinct values it backs, claim count, min/max/latest confidence, most-recent assertion (max transaction_from). Sufficient for a caller to apply its own trust policy; the engine never picks a winner (out of scope).

## 9. Output & LLM ergonomics (AC5)

Structured `ContradictionGenealogy` serializes to documented JSON; `narrative` is a deterministic prose summary ("N competing claims for `ceo` on Node(42): 'Alice' (source sec-filing, conf 0.9) diverged from 'Bob' (press-release, 0.75) at valid 2024-03-01, recorded 2024-06-01 — retroactive correction"). Bounded via `max_claims`/`max_sources` with `truncated` disclosure; composes with the #3353 token-budget ladder at the MCP layer. Errors follow #3234.

## 10. Determinism & read-only (AC6)

Pure over `(history, options)`; no writes, no mutation of history. Claims sorted by `(transaction_from, version_id)`, sources by `(source name, None last)`, values by display — byte-identical across runs and independent of input key order. Tests: `genealogy_is_byte_identical`, `genealogy_is_key_order_independent` (mirrors belief-revision).

## 11. MCP surface (designed, DEFERRED)

`contradiction_genealogy` and `find_contradictions` MCP tools are designed here but their registry wiring is **deferred to the MCP-registry batch follow-up** (lane policy: one registry-changing PR at a time). Shapes mirror `get_belief_revisions`: reader-class, feature-gated with a `#[cfg(not(feature="semantic-temporal"))]` `FAILED_PRECONDITION` twin, JSON serializer alongside `belief_revision_log_to_json`. This PR lands the complete Rust API + tests + bench; MCP lands next.

## 12. Alternatives considered

- **Cohort placement:** `semantic-diagnostics` (home of the existing Dissonance/Polygraph contradiction detectors) vs `semantic-temporal` (home of belief-revision, whose `RevisionClass` classifier this feature reuses for AC3). Chosen: **`semantic-temporal`** — the deepest reuse (classifier + history walk) lives there, avoiding a cross-cohort feature dependency; the feature is fundamentally about contradictions *over bi-temporal history*.
- **Write-time contradiction index (Approach B):** O(1) scans but requires a write-path/WAL change — violates read-only scope + AC6 and the #3413 WAL freeze. Deferred (future incremental-index follow-up).
- **Query-language predicate (Approach C):** an AQL/Cypher `CONTRADICTS` operator — belongs to the #3354 provenance-predicate workstream; deferred.

## 13. Risks / edge cases → test matrix

1. No contradiction: single version / retracted-then-non-overlapping succession → empty.
2. Backdated correction → 1 contradiction, RetroactiveCorrection, correct divergence coord.
3. Forward unretracted overlapping change → ContemporaneousDisagreement.
4. Retracted prior (closed valid_to, no overlap) → not flagged.
5. Three-way chain → 1 contradiction, 3 claims, 3 pairs, earliest divergence.
6. Re-asserted equal value (reaffirmation) → not flagged.
7. Unrelated property untouched → not flagged.
8. Missing provenance on some claims → unattributed source bucket, not dropped.
9. Multiple sources back same/different values → source summary correct.
10. Explicit `Claims` target spanning two entities → genealogy over given versions.
11. Determinism: byte-identical + key-order independent.
12. Pagination: > limit contradictions → has_more/next_offset, no dup/gap.
13. Elision: `max_claims` caps claims, `truncated=true`, narrative honest.
14. `as_of_transaction_time`: excludes later-recorded claims (time-travel).
15. Edge-property contradiction parity.
16. Errors: entity not found → NOT_FOUND; empty Claims → INVALID_ARGUMENT.
17. Value types Int/Float/String/Bool; Float NaN caution.
18. Perf: ≤1K versions p99 < 50ms (bench `benches/contradiction_genealogy.rs`).
19. `find_contradictions` 100% recall/precision on seeded label-scan fixture.
20. Narrative non-empty; names divergence + sources.

## 14. Graduation checklist (ADR-0050)

- [ ] Rust API stable across ≥1 minor cycle with no breaking changes.
- [ ] MCP tools shipped (registry batch) + docs guide.
- [ ] Bench meets p99 < 50ms (≤1K versions) and find_contradictions < 10s / 100K-node 1%-conflict fixture.
- [ ] Agent-eval ≥ 80% correct adjudication vs documented no-tool baseline.
- [ ] Compiles standalone (`just check-features`); clippy clean on CI feature set, `--all-features`, `--no-default-features`.
- [ ] Graduate flag from `semantic-temporal` experimental into a stable cohort per ADR-0050 pattern.

## 15. Acceptance-criteria mapping

| AC | Where satisfied |
|----|-----------------|
| 1 genealogy (valid/tx/provenance/supersession/divergence) | §5,§6 `ContradictionGenealogy`; MCP designed §11 (deferred) |
| 2 falsifiable detection + scan | §3,§7 `find_contradictions`; test 19 |
| 3 retroactive vs contemporaneous | §4 `DivergenceKind`; test 2,3 |
| 4 per-source aggregation | §8 `SourceSummary`; test 8,9 |
| 5 LLM-consumable + narrative + #3234 | §9; test 13,20 |
| 6 read-only + deterministic | §10; test 11 |
| 7 experimental flag + graduation | §1,§12,§14 |

Report the PR number and URL when done.
