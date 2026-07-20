# Trust Propagation: Computed Confidence over Derivation Lineage (#3382)

Status: DRAFT / design locked, implementation pending
Owner: Mark Masterson
Issue: [#3382 — Add trust propagation: infer confidence across derived facts](https://github.com/autumn-foundation/AletheiaDB/issues/3382)
Depends on: #3371 (derivation lineage, landed), #3224 (declared provenance confidence, landed)
Feeds: #3348 (declared-confidence query filters), #3372 (provenance-weighted retrieval)
Feature gate: `semantic-reasoning` (experimental "Nova" cohort) — zero overhead when disabled.

---

## 1. Problem & scope

Declared provenance confidence (#3224) stops at the first hop: a fact written
directly from a source carries that source's confidence, but the facts agents
actually consume are *derived* — a summary distilled from ten documents, an
entity merged from three records, an inference chained across prior inferences.
Today a derived fact's confidence is whatever its writer typed (invented, stale,
or missing) and it never updates when the evidence moves. Derivation lineage
(#3371) records the *structure* of evidence; **trust propagation computes over
it**: derived facts carry a **computed** confidence, inferred from upstream
evidence through a declared, deterministic combination policy, with an
explainable per-fact breakdown and recomputation when evidence changes.

This document locks the design. It computes over lineage; it does **not** record
lineage (that is #3371). Out of scope (verbatim from the issue): recording
lineage structure (#3371); declared-provenance filtering (#3348) and
similarity-fusion mechanics (#3372) — we supply the computed signal they
consume; user-defined/pluggable combinators (v1 is the built-in set); full
probabilistic-database (possible-worlds) semantics — combinators are explicit,
documented approximations that treat evidence as independent per policy;
automatic actions on low trust; cross-database/federated trust exchange.

### 1.1 Acceptance criteria (verbatim from issue #3382)

1. A trust-propagation policy can be enabled per database (or per label scope)
   declaring how confidence combines across lineage: v1 ships at least two
   documented, deterministic built-in combinators (e.g. a conservative
   weakest-link/min rule and a noisy-OR-style independence rule); the active
   policy is discoverable via API/MCP. Policy semantics are specified precisely
   enough that expected values are hand-computable.
2. For any fact with declared lineage (#3371), the engine can return its
   **computed confidence**: a deterministic function of the pinned upstream
   versions' confidences (declared for root facts per #3224, computed for
   intermediate derivations) under the active policy. Facts without lineage keep
   their declared confidence unchanged; declared and computed confidence are
   distinct, separately readable fields — computation never overwrites what a
   writer asserted.
3. **Explainability is mandatory**: a `trust_breakdown` API/MCP call returns,
   for a given fact, the computation tree — each upstream fact's contribution,
   its own confidence (declared or computed), the combinator applied at each
   node, and the resulting value — bounded by depth/size limits with the
   standard truncation signals (#3226). On a hand-constructed fixture (3-level
   lineage, mixed confidences), the breakdown matches the hand-computed tree
   exactly, node by node.
4. **Evidence changes flow downstream**: when an upstream fact's confidence
   changes (a superseding write with new provenance) or an upstream fact is
   retracted (#3230), the computed confidence of transitively derived facts
   reflects it — either recomputed lazily at read time or eagerly with a
   documented staleness bound (Engineering's call; the contract is that a read
   after the documented bound returns the updated value). Retracted evidence
   contributes per an explicit documented rule (e.g. contribution drops to
   zero), never silently retains its old weight.
5. **Bi-temporal honesty**: computed confidence is time-travelable — asking for
   a fact's computed confidence AS OF transaction time T evaluates the policy
   over the lineage and confidences as recorded at T, so "how confident were we
   then?" is answerable and revision-stable. Recorded history is never mutated
   by recomputation (temporal invariants untouched).
6. Cycles are impossible by construction (#3371 rejects them); missing-confidence
   evidence (roots written without confidence) contributes per an explicit
   documented rule and is flagged in the breakdown, never silently defaulted.
7. Computed confidence is queryable as a predicate (filter reads/traversals by
   computed-confidence threshold, composing with #3348's declared-confidence
   filters) and eligible as a fusion signal for provenance-weighted retrieval
   (#3372).
8. Ships under an experimental `semantic-*` cohort feature flag: zero write-path
   or read-path overhead when disabled (bench-verified).

### 1.2 Success metrics (verbatim)

- Correctness: 100% exact match between computed confidences/breakdowns and
  hand-computed expected values across a CI fixture covering both combinators,
  retraction, missing-confidence roots, and 5-level chains.
- Read cost: computed-confidence read for a fact with a ≤100-node lineage
  closure completes < 10ms p99 (within the temporal-reconstruction budget);
  single-fact declared-confidence reads are unaffected.
- Freshness: after an upstream confidence change, 100% of transitively derived
  facts return updated computed confidence within the documented staleness
  bound, verified on a 10K-fact lineage fixture.
- Capability proof: "which currently-served conclusions fall below 0.5 computed
  confidence if source X is downgraded to 0.2?" answerable in ≤ 3 calls.

---

## 2. Design decisions (LOCKED)

### 2.1 Feature gate & module placement

The whole feature lives under the **`semantic-reasoning`** experimental (Nova)
cohort flag — NOT default, NOT `semantic-search`. Zero write-path/read-path
overhead when the flag is off (AC8, bench-verified). Modules live under the
existing reasoning tree: `src/experimental/reasoning/` (that directory is where
every `#[cfg(feature = "semantic-reasoning")]` module already lives, e.g.
`hindsight.rs`, `metaphor.rs`, `muse.rs`, `luna.rs`, `omen.rs`, `prophet.rs`).
Proposed new module: `src/experimental/reasoning/trust.rs` (plus a
`trust_policy` submodule if it grows). Each item is `#[cfg(feature =
"semantic-reasoning")]`; a `#[cfg(not(...))]` shim mirrors the sibling modules
so the crate still builds with the flag off. Must pass `just check-features`
(compiles standalone).

### 2.2 Lazy recompute — no stored score

**Decision (AC4): computed confidence is NEVER stored.** It is computed on read
by walking `upstream_lineage` (the #3371 in-memory `LineageStore` closure) and
combining upstream confidences under the active policy. Rationale:

- Evidence changes (a superseding write with new provenance, or a #3230
  retraction) flow downstream **for free** — the next read recomputes from
  current state. Staleness bound is therefore **zero** (read-time freshness),
  the strongest form of the AC4 contract.
- No write-path overhead (AC8): the write path is untouched; the WAL format is
  untouched (mirrors #3371's own in-memory-only v1 stance).
- No recorded history is mutated (AC5) — there is nothing to mutate.

Cost is bounded by lineage-closure size; the ≤100-node <10ms p99 target (Success
Metric) is met because the closure walk reuses the existing `LineageStore`
adjacency `DashMap`s (in-RAM, O(closure)) plus one `provenance.confidence()` read
per referenced version from historical storage. **Per-read memoization** (§2.7)
keeps a diamond-shaped DAG from re-evaluating a shared ancestor.

### 2.3 The two combinators (EXACT formulas)

Given the set of *contributing* upstream confidences `c_1..c_n ∈ [0,1]` at a
derivation node:

- **Weakest-link** (conservative / pessimistic): `computed = min(c_1..c_n)`.
  A chain is only as strong as its weakest evidence. Empty contributing set
  → see leaf rules (§2.6).
- **Noisy-OR** (independence / corroboration): `computed = 1 − ∏_i (1 − c_i)`.
  Independent evidence corroborates: more supporting facts raise confidence.
  Empty contributing set → leaf rules (§2.6).

Both are deterministic, order-independent, and hand-computable (AC1). Worked
example (a node with sources 0.9, 0.9, 0.3):
- weakest-link → `min = 0.3`.
- noisy-OR → `1 − (0.1)(0.1)(0.7) = 1 − 0.007 = 0.993`.

These are explicit approximations assuming evidence independence (documented as
such per the Out-of-Scope probabilistic caveat).

### 2.4 Per-label trust policy + durable sidecar registry

A **`TrustPolicy`** declares the active combinator and the missing-confidence
rule. Resolution order per fact (most specific wins):

1. Per-label / per-edge-type override (keyed by the derived fact's label).
2. Per-database default combinator.

Discoverable via **`list_trust_policies`** (API + deferred MCP) — satisfies the
AC1 "active policy is discoverable" requirement.

**Durable sidecar** `trust_policy.json`, mirroring the schema-constraints
(`schema_constraints.dat`) / snapshots (`snapshots.json`) pattern in
`src/db/snapshot.rs` (`SnapshotRegistry`):

- Atomic write: serialize → write temp (`path.with_extension("tmp")`) →
  `file.sync_all()` → `std::fs::rename(tmp, path)` → parent-dir `fsync` (unix)
  (verbatim the `save_serialized` dance).
- **Tolerant load**: a missing/truncated/corrupt/unknown-future-version file
  must NOT brick startup — warn, quarantine aside to `*.corrupt` (preserving
  bytes), start with an empty registry (verbatim the snapshot registry's
  deliberate divergence from the auth key store).
- A `version: u32` format tag in the persisted struct (bump-on-incompatible).
- Location: inside the persistence dir, e.g.
  `{persistence.data_dir}/trust_policy.json` (or `{data_dir}/indexes/...` under
  the durable config), only when index persistence is enabled.
- **Ephemeral `AletheiaDB::new()` → in-memory only**, no sidecar (matches the
  snapshot registry's `persist_path: None` branch).
- Folded into the `.albk` backup payload is a **follow-up** (schema constraints
  are; #3218 uniqueness is not — note the residue, do not block on it).

### 2.5 Core types

```text
// src/experimental/reasoning/trust.rs  (all #[cfg(feature = "semantic-reasoning")])

/// The two readable confidence fields for a fact — never conflated (AC2).
struct ComputedConfidence {
    /// The writer-declared value from provenance (#3224), untouched. `None`
    /// when the writer asserted no confidence. Computation NEVER overwrites
    /// `provenance.confidence()`.
    declared: Option<f64>,
    /// The value computed from upstream evidence under the active policy.
    /// `None`/flagged when the fact has no lineage (then `declared` stands) or
    /// when the missing-confidence rule yields no value.
    computed: Option<f64>,
    /// Which combinator produced `computed`, and the missing-confidence rule in
    /// force, so the number is self-describing.
    policy: TrustPolicyRef,
    /// True when any contributing upstream was missing-confidence / absent /
    /// retracted and a rule was applied — surfaces the AC6 "always flagged".
    flagged: bool,
}

/// Per-upstream classification driving its contribution. DISTINCT variants —
/// `Absent` (dangling / deleted from current state) is NOT collapsed into
/// `Retracted` (valid-time-closed via #3230). (Review-fix #2.)
enum ConfidenceSource {
    Declared(f64),      // root fact with #3224 confidence
    Computed(f64),      // intermediate derivation, recursively computed
    MissingConfidence,  // root written without confidence (AC6)
    Retracted,          // #3230 valid-time retraction — contributes 0.0, dominates
    Absent,             // deleted / dangling in current state — contributes 0.0, dominates
}

/// One node of the explainable computation tree (AC3).
struct TrustBreakdownNode {
    reference: LineageRef,        // entity + pinned version (#3371)
    source: ConfidenceSource,
    contribution: f64,            // value fed into the parent combinator
    combinator: Combinator,       // applied at THIS node over its children
    value: f64,                   // this node's resulting confidence
    children: Vec<TrustBreakdownNode>,
    truncated: bool,              // subtree elided by depth/size cap (#3226)
}

enum Combinator { WeakestLink, NoisyOr }

/// The missing-confidence resolution rule (AC6) — explicit, per-policy, never
/// a silent default.
enum MissingConfidenceRule {
    Zero,     // treat as 0.0 (pessimistic)
    Neutral,  // treat as a documented neutral constant (e.g. 0.5)
    Ignore,   // drop from the contributing set (does not affect the combinator)
}

struct TrustPolicy {
    default: Combinator,
    missing: MissingConfidenceRule,
    per_label: HashMap<String, Combinator>, // label / edge-type override
}
```

`ComputedConfidence.computed` is derived; `declared` is a straight read of
`provenance.confidence()` (§ `src/core/provenance.rs`, `Provenance::confidence()
-> Option<f64>`). The two are separately readable (AC2).

### 2.6 Leaf & status rules (LOCKED)

Base case — a fact with **no lineage record** (`LineageStore::record_for`
returns `None`): its computed confidence IS its declared confidence
(`provenance.confidence()`); no combination happens; `computed = declared`.
Facts without lineage keep their declared confidence unchanged (AC2).

Per-upstream contribution, by `ConfidenceSource` (resolved against #3371's
`FactStatus` + `provenance`):

| Upstream state | `ConfidenceSource` | Contribution |
|---|---|---|
| Root, has #3224 confidence | `Declared(c)` | `c` |
| Intermediate derivation | `Computed(c)` | recursively computed `c` |
| Root, no confidence written | `MissingConfidence` | per `MissingConfidenceRule` (zero/neutral/ignore), **flagged** |
| #3230 valid-time-retracted (`FactStatus::Absent` via retraction tombstone) | `Retracted` | **0.0, dominates** |
| Deleted / dangling in current state | `Absent` | **0.0, dominates** |

"Dominates": when **any** contributor at a node resolves to `Retracted` or
`Absent`, the node **short-circuits to 0.0** — retraction/absence dominates under
BOTH combinators. This is implemented by an **explicit terminal-child check** in
the node combine step (the node knows each child's terminal status and, if any
child is terminal, forces its own computed value to `0.0` and sets
`has_retracted_inputs`), **NOT** by relying on `0.0` being a noisy-OR identity
term. That distinction is load-bearing: under noisy-OR `1 − ∏(1 − c)` a `0.0`
term is the *identity* `(1 − 0) = 1`, so a retracted `0.0` contributed
positionally would be **silently absorbed** by a live sibling (e.g.
`noisy_or{retracted 0.0, live 0.9} = 0.9`) — the earlier "recursion stops so it
is not absorbed" justification was mathematically false. The explicit
terminal-child cap is what makes domination hold under noisy-OR. `combine_values`
stays a pure combinator (it never sees terminal status); the domination decision
lives entirely in the node combine step. Domination is **local** to the node with
the terminal contributor: the resulting `0.0` then flows to the parent as an
ordinary value (a parent's noisy-OR may legitimately corroborate that dead `0.0`
with other live evidence), while `has_retracted_inputs` bubbles up the whole
subtree. Pinned in §5 test cases R-1 / R-2 / R-2b.

**`Retracted` vs `Absent` are DISTINCT** (review-fix #2): both contribute 0.0
today, but they are separate `ConfidenceSource` variants and separate breakdown
labels so the explanation distinguishes "we withdrew this as of a valid time"
from "this is gone / dangling" — and so a future policy can treat them
differently without a format change. #3371's `FactStatus` already distinguishes
`Superseded` from `Absent`; we map: live+current/superseded → its confidence;
absent-via-retraction → `Retracted`; absent-via-delete → `Absent`.

### 2.7 Cycle & depth safety (review-fix #3)

Cycles are impossible by construction — #3371's `LineageStore::record` rejects
self-derivation and cycles (version-space is a DAG; edges point strictly from
higher to lower `VersionId`). We still add defence-in-depth so a pathological or
future-relaxed graph cannot overflow the stack or run away:

- **Hard depth cap**: reuse `LineageQueryOptions::max_depth`
  (`DEFAULT_MAX_DEPTH = 32`) as the recursion bound; beyond it the subtree is
  marked `truncated` (#3226) and the node's own value is still computed from
  what was reached.
- **DFS visited/stack guard**: a `HashSet<VersionId>` on the current DFS path
  aborts on re-entry (belt-and-braces vs. the DAG guarantee).
- **Per-version memoization**: a `HashMap<VersionId, f64>` of already-computed
  values so a diamond DAG evaluates each shared ancestor once (correctness +
  the <10ms/≤100-node budget).
- Iterative/explicit-stack option kept open if recursion depth is a concern; the
  depth cap makes native recursion safe regardless.

### 2.8 Truncated-breakdown must not over-trust (review-fix #1)

When the breakdown tree is truncated by a depth/size cap (`has_more` / node
`truncated: true`, #3226), the returned **breakdown** is incomplete but the
node's **computed confidence value stays full-accuracy**: computation walks the
lineage independently of the *presentation* size cap, so a truncated explanation
never inflates (or deflates) the reported number. Concretely: the breakdown-tree
size limit governs how many `TrustBreakdownNode`s we *serialize*, NOT how many
upstream confidences we *combine*. If a hard combination bound is ever hit
(pathological closure beyond `max_depth`), the missing inputs must be treated
conservatively (clamp / flag) — never as "absent = no effect" that would
over-trust the incomplete set. This is a dedicated test case (§5 T-1/T-2).

### 2.9 Bi-temporal AS OF

`computed_confidence_as_of(ref, T)` evaluates the policy over lineage +
confidences **as recorded at transaction time T** (AC5):

- Lineage closure is already `AS OF`-scopable: `LineageQueryOptions::with_as_of(T)`
  filters to records with `recorded_at <= T` (see `neighbours_upstream`/
  `neighbours_downstream`). We pass `T` straight through.
- Each contributing reference's confidence is read from the entity's **head as
  recorded at `T`** — `resolve_ref` walks the entity's version chain to the
  latest version whose transaction-time start `<= T` and reads *that* version's
  provenance confidence. The version pinned in the `LineageRef` sets only the
  reference's Current-vs-Superseded status; the confidence itself is NOT read
  from the pinned `reference.version` (correctness L1). This is what makes the
  now-eval reactive (head = current) and the as-of eval revision-stable (head =
  as recorded at `T`).
- **Valid-time terminality is keyed on wallclock `time::now()`**, NOT on the
  as-of `T`: the `AS OF` coordinate scopes **transaction-time only**. A fact
  whose valid interval has *ended as of now* (`valid_to <= now`) is `Retracted`;
  a fact retracted **effective-future** (still valid now) or holding a
  naturally-bounded interval that currently **contains** now is **live** and
  contributes its confidence (Group 2 fix). Using wallclock now for both
  `computed_confidence` and `computed_confidence_as_of` is a **documented
  approximation** for the as-of path (a fully valid-time-scoped trust evaluation
  — replaying valid-time terminality as it stood at `T` — is a tracked
  follow-up). A version that merely **predates** `T` (not yet recorded at `T`) is
  `Absent`, contributes `0.0`, but is NOT a retraction and does not flag
  `has_retracted_inputs` (adversarial #7).
- Recorded history is never mutated (nothing is written; §2.2).
- **No-op AS OF (review-fix #4)**: `computed_confidence_as_of(ref, T)` with `T`
  at or after the latest transaction time MUST equal the unscoped
  `computed_confidence(ref)`. Regression-guarded (§5 A-3).

### 2.10 Predicate & fusion (AC7)

- **`ComputedConfidenceFilter`** — a predicate `computed_confidence >= threshold`
  (and `<`, range) usable on reads/traversals, composing with #3348's
  `ProvenanceFilter` (declared-confidence, see `src/core/provenance.rs`
  `ProvenanceFilter` / `min_confidence`). Both filters AND together.
- Computed confidence is exposed as a **fusion signal** for #3372
  provenance-weighted retrieval (glue behind both feature flags). Mechanics of
  fusion are owned by #3372; we only supply the signal.

### 2.11 MCP surface — DESIGNED, DEFERRED

Per the one-registry-PR rule, the MCP `trust_breakdown` tool, a
`computed_confidence` read field, `list_trust_policies`, and the predicate
params are **designed here but DEFERRED** to the MCP registry batch. Shape
(for the later PR): `trust_breakdown` (reader-class) args
`{entity_kind, id, version?, max_depth?, limit?, as_of_transaction_time?}` →
the `TrustBreakdownNode` tree + `has_more`; errors use the #3234 structured
codes (`NOT_FOUND` dangling root, `FAILED_PRECONDITION` when the
`semantic-reasoning` feature is absent → `required_feature`, `INVALID_ARGUMENT`
bad bound), all non-retriable. This wave is **Rust-API-only** (mirrors #3370's
Rust-API-only first wave).

---

## 3. Public API surface (Rust, `semantic-reasoning`-gated)

```text
impl AletheiaDB {
    // policy management (durable sidecar)
    fn set_trust_policy(&self, policy: TrustPolicy) -> Result<()>;
    fn set_label_trust_policy(&self, label: &str, combinator: Combinator) -> Result<()>;
    fn list_trust_policies(&self) -> TrustPolicyView;         // AC1 discoverable
    fn drop_label_trust_policy(&self, label: &str) -> Result<()>;

    // computed confidence (lazy, AC2/AC4)
    fn computed_confidence(&self, root: LineageRef) -> ComputedConfidence;
    fn computed_confidence_as_of(&self, root: LineageRef, t: Timestamp) -> ComputedConfidence; // AC5

    // explainability (AC3)
    fn trust_breakdown(&self, root: LineageRef, options: LineageQueryOptions) -> TrustBreakdown;
}

// AC7 predicate
struct ComputedConfidenceFilter { min: Option<f64>, max: Option<f64> }
```

Built on the existing #3371 surface (do not re-derive):
- `LineageRef { entity: EntityId, version: VersionId }`, `LineageRef::new(...)`.
- `AletheiaDB::upstream_lineage(root, options) -> LineageView`,
  `downstream_lineage(root, options) -> LineageView`.
- `LineageView { root, entries: Vec<LineageViewEntry{reference, depth, status}>, has_more }`.
- `FactStatus { Current, Superseded, Absent }`.
- `LineageQueryOptions { max_depth, limit, as_of }` with `with_as_of(T)`,
  `with_max_depth`, `with_limit`; `DEFAULT_MAX_DEPTH = 32`, `DEFAULT_LIMIT = 1000`.
- `LineageStore::record_for(version) -> Option<LineageRecord>`,
  `direct_upstream(version) -> Vec<LineageRef>`.
- Confidence source: `Provenance::confidence() -> Option<f64>` (`src/core/provenance.rs`).

---

## 4. Twenty-case test matrix

Combinator / correctness (AC1, AC2, AC3):
1. **C-1 weakest-link basic** — node over {0.9, 0.9, 0.3} → 0.3 exactly.
2. **C-2 noisy-OR basic** — node over {0.9, 0.9, 0.3} → 0.993 exactly.
3. **C-3 single upstream** — both combinators pass through the one value.
4. **C-4 3-level chain weakest-link** — hand-computed tree matches node-by-node (AC3).
5. **C-5 3-level chain noisy-OR** — hand-computed tree matches node-by-node (AC3).
6. **C-6 5-level chain** — Success-Metric depth; exact match both combinators.
7. **C-7 diamond DAG** — shared ancestor combined once (memoization correctness).
8. **C-8 per-label override** — two labels, different combinators, resolved per fact.
9. **C-9 no-lineage fact** — computed == declared, no combination (AC2).
10. **C-10 declared vs computed distinct** — computation does not overwrite
    `provenance.confidence()` (read both fields, both intact) (AC2).

Leaf / status rules (AC6, review-fix #2):
11. **M-1 missing-confidence Zero rule** — root w/o confidence → 0.0, flagged.
12. **M-2 missing-confidence Neutral rule** — → neutral constant, flagged.
13. **M-3 missing-confidence Ignore rule** — dropped from set, combinator over rest, flagged.
14. **R-1 retracted upstream weakest-link** — `Retracted` → 0.0 dominates min.
15. **R-2 retracted upstream noisy-OR** — single `Retracted` source caps the node
    to 0.0 (does not vanish as identity term).
    **R-2b multi-source noisy-OR domination** — `noisy_or{retracted, live 0.9}` is
    0.0, NOT 0.9: the explicit terminal-child cap prevents the live sibling from
    absorbing the retracted 0.0 as the noisy-OR identity term.
16. **R-3 absent (deleted) upstream** — `Absent` → 0.0, DISTINCT variant/label from `Retracted` (review-fix #2).

Truncation (review-fix #1):
17. **T-1 truncated breakdown, value intact** — depth/size cap sets `truncated`/`has_more`,
    but `computed` value equals the untruncated computation (no over-trust).
18. **T-2 hard combination bound conservative** — inputs beyond the bound are
    clamped/flagged conservatively, never treated as absent-no-effect.

Depth / cycle safety (review-fix #3):
19. **D-1 depth-cap guard** — chain deeper than `max_depth` returns truncated,
    no overflow/panic; DFS stack guard + memoization exercised.

Bi-temporal (AC5, review-fix #4):
20. **A-1 AS OF earlier** — confidence as recorded at an earlier T differs from now.
    **A-2 AS OF reactive-at-now** — a superseding write changes the unscoped value.
    **A-3 no-op AS OF** — `computed_confidence_as_of(T≥latest)` == unscoped
    `computed_confidence` (review-fix #4 regression guard).

(Cases A-1/A-2/A-3 are grouped under matrix slot 20; 22 concrete asserts across
20 named cases. `just check-features` compiles the `semantic-reasoning` module
standalone; AC8 zero-overhead is a `#[cfg(not(feature))]` bench guard.)

### 4.1 The four review fixes, mapped

| Review fix | Design section | Dedicated test |
|---|---|---|
| #1 truncated-breakdown over-trust | §2.8 | T-1, T-2 |
| #2 Retracted vs Absent distinct | §2.6 (`ConfidenceSource`) | R-1, R-2, R-3 |
| #3 unbounded-depth overflow guard | §2.7 | D-1 |
| #4 no-op AS OF equals unscoped | §2.9 | A-3 |

---

## 5. Inferences & open flags

Things inferred (NOT verbatim from the issue), called out for review:
- **Lazy-only, zero staleness** (§2.2): the issue leaves eager-vs-lazy to
  Engineering; we lock lazy (read-time) — strongest freshness, no write overhead.
- **Module path** `src/experimental/reasoning/trust.rs` — inferred from where
  `semantic-reasoning`-gated modules live; confirm at implementation.
- **Neutral constant = 0.5** for `MissingConfidenceRule::Neutral` — a documented
  choice, not from the issue; finalize in the guide.
- **Noisy-OR domination of retracted/absent** (§2.6): the issue says retracted
  "contribution drops to zero"; because 0.0 is the noisy-OR identity, an explicit
  terminal-child cap in the node combine step (not a positional 0.0) forces the
  node to 0.0 so a retracted input is not silently absorbed by a live sibling.
  Pinned by R-2 / R-2b; revisit if a non-dominating interpretation is preferred.
- **Sidecar filename** `trust_policy.json` and its dir — mirrors the snapshot
  registry; confirm against `PersistenceConfig` at implementation.

This document is the anchor; implementation (Rust API + tests + guide
`docs/guides/trust-propagation.md`) follows in the same branch. MCP surface is a
deferred follow-up (§2.11).

---

## Locked API Surface (implementation reference)

REFERENCE ONLY — rewrite all code fresh during implementation; never reuse
commit-message text. This is the recovered LOCKED surface, reproduced verbatim
so it survives in the pushed branch (the scratchpad does not survive a container
reclaim). Reconciliation notes against issue #3382 ACs follow at the end
(§ "Reconciliation with ACs"); where this surface and §2 differ, the differences
are flagged there rather than silently resolved.

**WIRING**: `src/db/mod.rs` adds field
`#[cfg(feature = "semantic-reasoning")] pub(crate) trust_policies: Arc<TrustRegistry>`
(a leaf like `snapshots`, off the data write path). `src/db/config.rs`
constructs it in BOTH constructors: durable via
`TrustRegistry::open(registry_path_for(&config.persistence))?`, ephemeral via
`TrustRegistry::in_memory()`. `registry_path_for(persistence)` → `None` if
`!persistence.enabled` else `Some(persistence.data_dir.join("trust_policy.json"))`.
`src/experimental/reasoning/mod.rs` adds `pub mod trust_propagation;`. (Verify
these module paths exist under the `semantic-reasoning` tree; adjust to the real
layout if it differs and note it. VERIFIED at anchor time:
`src/experimental/reasoning/` exists and is the home of every
`#[cfg(feature = "semantic-reasoning")]` module — module file will be
`src/experimental/reasoning/trust_propagation.rs`.)

**CONSTANTS**: `PERSIST_FORMAT_VERSION: u32 = 1` (serde-gated);
`DEFAULT_MAX_DEPTH = 32`; `DEFAULT_MAX_NODES = 1000`; `SCALAR_MAX_DEPTH = 1024`
(private); `NEUTRAL = 0.5` (private).

**TYPES** (all in `trust_propagation.rs`, serde derives cfg-gated on `"serde"`,
snake_case renames):
- `enum TrustCombinator { WeakestLink, NoisyOr }` + `const fn as_str()` →
  `"weakest_link"`/`"noisy_or"`.
- `enum MissingConfidencePolicy { Zero, Neutral, Ignore }`.
- `struct TrustPolicy { combinator, missing }` + `const fn new`/`weakest_link`/
  `noisy_or`; `Default` = weakest_link + Zero.
- `enum ConfidenceSource { Declared, Computed, Missing, Retracted }` + `Absent`
  (distinct from `Retracted` — review fix #2).
- `struct ComputedConfidence { declared: Option<f64>, computed: f64,
  has_lineage: bool, combinator: Option<TrustCombinator>, has_missing_inputs:
  bool, has_retracted_inputs: bool, truncated: bool }`.
- `struct TrustBreakdown { reference: LineageRef, status: FactStatus,
  confidence: f64, source: ConfidenceSource, combinator: Option<TrustCombinator>,
  children: Vec<TrustBreakdown>, truncated: bool }` — NOT serde (embeds
  `LineageRef`/`FactStatus`); MCP projection deferred.
- `struct TrustOptions { max_depth, max_nodes, as_of: Option<Timestamp> }`,
  `Default` = (32, 1000, None).

**PURE CORE**: `combine_values(children: &[Option<f64>], combinator) -> (f64,
bool)` — `None` children excluded (Ignore); empty included set → `(NEUTRAL,
true)` and caller flags `has_missing` (never min-of-empty / empty-product);
WeakestLink = fold min; NoisyOr = `1 − ∏(1−c)`; result through `clamp01`
(NaN→0.0, clamp `[0,1]`).

**REGISTRY**: `TrustRegistry { state: RwLock<RegistryState{default, labels:
HashMap}>, persist_path: Option<PathBuf>, save_lock: Mutex<()> }` — `open()`
tolerant-load mirroring the #3370 snapshot registry: corrupt/unparseable/
future-version file quarantined to `*.corrupt` + warn, never bricks startup;
version accepted iff `<= PERSIST_FORMAT_VERSION`. `set_default`/`set_label` hold
`save_lock`, mutate, save via temp+fsync+rename+parent-fsync(unix), ROLL BACK the
in-memory change if save fails. `list()` sorts labels.

**EVALUATION SEMANTICS** (load-bearing):
- `AletheiaDB` methods: `trust_policy()`, `trust_policy_for_label(label)`
  (override else default), `list_trust_policies()`, `set_trust_policy(policy)`,
  `set_trust_policy_for_label(label, policy)`, `computed_confidence(ref)` = at
  `time::now()`, `computed_confidence_as_of(ref, tt)`,
  `node_computed_confidence(NodeId)`/`edge_computed_confidence(EdgeId)` (resolve
  current lineage ref; NodeNotFound/EdgeNotFound), `trust_breakdown(ref,
  &TrustOptions)`.
- `ensure_ref_exists` first: version lookup must match the entity else
  `StorageError::VersionNotFound` → `NOT_FOUND`.
- `visible_lineage(version, tt)`: lineage record filtered `recorded_at <= tt`.
- `resolve_ref(ref, tt)`: entity history → latest version with
  `transaction_time().start() <= tt`; absent = closed valid interval
  (retracted/deleted); status `Current` iff `version_id` matches pinned ref else
  `Superseded`; confidence from that VISIBLE version's provenance (makes
  now-eval reactive, as-of stable).
- `eval_scalar` with memo: `HashMap<VersionId, ScalarEval>` + `stack:
  HashSet<VersionId>` DFS guard + depth cap: retracted/absent → `Some(0.0)`,
  source Retracted/Absent, `has_retracted`, STOPS recursion (dominates). Leaf
  (no visible lineage): declared `Some(c)` → `clamp01(c)` Declared; `None` →
  label's missing policy (Zero→0.0 / Neutral→0.5 / Ignore→None excluded), always
  `has_missing`. Node with sources: each child recursed, combined under THIS
  node's label combinator (per-label mixing works), all-excluded → `NEUTRAL` +
  `has_missing`.
- `build_breakdown`: same shape, but when depth/budget/cycle stops descent, the
  node's confidence is a FULL fresh `eval_scalar` (scalar stays honest under
  truncation — the HIGH review fix #1), `truncated=true`, children empty. Node
  budget decrements per child; sibling loop breaks with `truncated` when
  exhausted.
- `Provenance::confidence` access via `.as_ref().and_then(Provenance::confidence)`.

Everything else (tests, e2e, fusion glue, predicate) rebuilds from the 20-case
matrix during implementation.

### Reconciliation with ACs

The locked surface above is authoritative for implementation; where it differs
from §2's earlier sketch or from an AC, the difference is noted here (no silent
pick):

- **`ComputedConfidence.computed: f64` (not `Option<f64>`)** — the locked
  surface makes `computed` always a concrete number, with `has_lineage: false`
  signalling "this equals the declared/leaf value, nothing was combined". §2.5's
  sketch used `computed: Option<f64>`. LOCKED: use `f64` + `has_lineage`.
  Consistent with AC2 (declared and computed are distinct, separately readable —
  `declared: Option<f64>` stays untouched from `provenance.confidence()`).
- **`PERSIST_FORMAT_VERSION = 1`** — the sidecar starts at format version 1
  (§2.4 spoke of a bump-on-incompatible `version` tag generically; locked value
  is 1). Load accepts `version <= 1`, quarantines higher/corrupt.
- **`MissingConfidencePolicy` field named `missing`** on `TrustPolicy`; §2.5
  called the enum `MissingConfidenceRule`. LOCKED name: `MissingConfidencePolicy`.
- **All-excluded / empty included set → `(NEUTRAL=0.5, true)`** rather than an
  error or min-of-empty. This is the documented "never min-of-empty /
  empty-product" rule and is always flagged (`has_missing`) — satisfies AC6
  ("flagged, never silently defaulted": the 0.5 is a *flagged* fallback, not a
  silent default). Note: AC6's *root* missing-confidence is governed by the
  per-policy `MissingConfidencePolicy` (Zero/Neutral/Ignore); the `NEUTRAL`
  fallback is the distinct case of a node whose entire contributing child set
  was Ignore-excluded.
- **Retracted/Absent dominate via an explicit terminal-child cap in the node
  combine step** — a retracted/absent contributor resolves to `0.0` and halts
  descent, but that alone is **not** sufficient under noisy-OR (where a `0.0`
  term is the identity and a live sibling would absorb it). So the node combine
  step, which knows each direct child's terminal status, explicitly forces its
  own value to `0.0` (and sets `has_retracted_inputs`) whenever ANY direct child
  is `Retracted`/`Absent` — under BOTH combinators. `combine_values` remains a
  pure combinator and is never asked to encode domination. The earlier
  "recursion stops so it is not absorbed" reasoning was mathematically false and
  is corrected here (see §2.6). Domination is local to the node holding the
  terminal contributor; the resulting `0.0` propagates to the parent as an
  ordinary value while `has_retracted_inputs` bubbles up. A fact that merely
  **predates** the evaluation `tt` (not yet recorded) resolves terminal-absent
  too but is NOT a retraction: it contributes `0.0` without dominating and
  without flagging `has_retracted_inputs` (adversarial #7).
- **`SCALAR_MAX_DEPTH = 1024`** private hard recursion ceiling is separate from
  the caller-facing `DEFAULT_MAX_DEPTH = 32` (`TrustOptions.max_depth`): the
  former is the overflow backstop (review-fix #3), the latter the default
  breakdown/closure bound. No AC conflict.
- **No-op AS OF (review-fix #4)** is preserved: `resolve_ref` uses the VISIBLE
  version at `tt`; with `tt >= latest transaction time` every visible version is
  the current one, so `computed_confidence_as_of(ref, tt)` == `computed_confidence(ref)`.
