# Issue #3372 — Provenance-Weighted Retrieval

**Status:** Draft / Wave-9 Lane C
**Feature flag:** `semantic-retrieval-fusion` (experimental cohort, ADR-0050)
**Complexity:** Tier L — no storage-format change (all inputs already recorded).

Fuse trust and recency into vector similarity so that k-NN / hybrid retrieval ranks
candidates by a tunable combination of **(a) vector similarity, (b) provenance
confidence, (c) temporal recency**, returning a per-result score breakdown that is
explainable and auditable instead of a single opaque number.

---

## 1. Problem statement

Pure vector similarity fills an LLM's context window with the geometrically closest
facts, which may be stale or low-trust. A grounded agent instead wants the most
*trustworthy-and-current* facts. All three signals are already recorded:

- similarity — the vector index score;
- confidence — `Provenance::confidence()` (`src/core/provenance.rs`), `[0,1]`;
- recency — the fact's `valid_from` (and/or `transaction_from`) on its
  `BiTemporalInterval`.

No new storage is needed — this is a **ranking** change over recorded inputs.

---

## 2. Brainstorming (candidate signals & shapes)

- Weighted arithmetic mean of the three normalized signals (simple, monotone, OR-ish).
- Weighted geometric mean (stricter AND-semantics: a zero in any signal tanks the score).
- Learned ranking / LTR (out of scope: issue defers to a separate spec).
- Trust propagation through lineage depth (out of scope: #3371).
- Per-source global trust registry (out of scope).
- Recency decay curves: linear-to-zero, step, exponential half-life. Exponential is
  smooth, monotone, bounded `(0,1]`, and has one intuitive knob (half-life).
- Confidence for missing provenance: drop (wrong — that is #3348 hard filtering),
  treat as 0 (punishes un-attributed facts), or a **configurable neutral** default
  (chosen — AC5).

## 3. Reverse brainstorming (how could this go wrong / be gamed?)

- **Post-hoc re-ranking masquerading as fusion.** If we only re-sort the similarity
  top-k, a high-trust candidate that is geometrically far never enters the shortlist.
  → We must **over-fetch a wide horizon** and fuse across it (AC4, the falsifiable core).
- **Opaque score.** A single fused number is un-auditable. → Always return the full
  breakdown (AC3).
- **Silent exclusion of un-attributed facts.** → Neutral-confidence default, never drop
  (AC5). Hard filtering is a *separate, composable* step (#3348): filter first, fuse
  survivors.
- **Non-determinism.** Floating point + wallclock `now` could make replays differ.
  → `fuse()` is a pure function of `(recorded inputs, policy, reference-now)`; recency
  takes an explicit `reference_now` so an `AS OF` / pinned snapshot replays identically
  (AC6).
- **NaN poisoning the sort.** A NaN similarity/confidence/recency could make the sort
  intransitive. → Clamp every component to `[0,1]` with NaN→0, and use a total-order
  comparator with a stable `node_id` tie-break.
- **Gaming via future-dating.** A `valid_from` in the future would give `age < 0`.
  → Clamp `age` at `0` so recency ≤ 1.
- **Invalid weights.** Negative / all-zero / NaN weights make the weighted mean
  undefined (division by zero). → Reject at policy construction with a structured error
  (AC7).
- **Monotonicity violation.** A caller must be able to trust "more confidence ⇒ not
  worse". → Weighted mean with non-negative weights is monotone non-decreasing in each
  component; enforced by a property test.

## 4. Six-hats

- **White (facts):** three recorded signals; over-fetch template already exists in
  `handle_find_similar` (#3348); metadata fetch seam
  `get_node_version_read_metadata(version_id) -> (Option<Provenance>, BiTemporalInterval)`.
- **Red (feelings):** callers want an *explainable* ranking they can cite; a black-box
  score erodes trust more than pure similarity does.
- **Black (caution):** over-fetch is horizon-bounded (`fused_horizon(k)` =
  `min(max(3·k, 1000), FUSION_MAX_HORIZON=10_000)`), so AC4's guarantee holds only for a
  target within the top `FUSION_MAX_HORIZON` similarity ranks — same caveat #3348
  documents, made `k`-scaled and explicitly capped. Latency: an extra metadata read per
  candidate + an O(n log n) sort.
- **Yellow (benefit):** maximal reuse, zero storage change, byte-for-byte no-op when the
  policy is omitted, composes with #3348 filtering and #3370 snapshots for free.
- **Green (creative):** a pure `fuse()` core shared by the Rust API and (later) both MCP
  handlers; graduation = flip one `#[cfg]` gate name.
- **Blue (process):** TDD — RED (one test per AC + risk) → GREEN → REFACTOR; ship behind
  an experimental flag with a documented graduation checklist.

---

## 5. Candidate approaches & tradeoffs

### Approach 1 — Handler/DB-level post-candidate re-scoring over an over-fetched horizon (CHOSEN)
Force the full-horizon over-fetch when a policy is present, build each candidate's
confidence + recency from the existing metadata seam, compute
`fused = fuse(sim, conf|neutral, recency, weights)`, sort by fused desc, then page.
- **Pros:** reuses the proven #3348 over-fetch (AC4 by construction); zero storage/index
  change; provenance+recency already resolved at the temporal coordinate on the
  `*_at_time` paths (AC6 nearly free); byte-identical when omitted (AC2); composes after
  `ProvenanceFilter` (AC5).
- **Cons:** over-fetch is `MAX_VECTOR_K`-bounded; the Rust `similarity_search` returns bare
  `(NodeId, f32)`, so a parallel fused entry point is needed.

### Approach 2 — Push the scorer into the vector layer / `SimilarityQuery`
The db returns fused-ordered `Vec<FusedHit>`; handlers just serialize.
- **Pros:** one implementation shared by Rust + MCP.
- **Cons:** pushes provenance/recency reads into the vector hot path; larger blast radius;
  changing `similarity_search`'s return type is not purely additive.

### Approach 3 — Planner/query-language fusion operator
- **Pros:** would unify AQL/Cypher. **Cons:** explicitly out of scope (#3354); heaviest.

### Decision
**Approach 1 for the surfaces, plus a thin Approach-2 slice for the Rust API:** a new,
**additive** `AletheiaDB::similarity_search_fused(query) -> Result<Vec<FusedHit>>` and a
`SimilarityQuery::fusion(policy)` builder, both sharing one `fuse()` core / one
`FusionPolicy` / `FusionBreakdown` type. The legacy `similarity_search` is left untouched
so AC2 (byte-for-byte no-op when the policy is omitted) holds trivially.

---

## 6. Scoring model (documented, deterministic, monotone)

Let `s` = similarity, `c` = confidence, `r` = recency, each normalized to `[0,1]`:

- `s` = the index's **cosine** similarity score, already in `[0,1]` for non-negative
  embeddings (negative values clamp to 0, NaN→0). This is a **Cosine-only v1 contract**:
  `similarity_search_fused` rejects a non-Cosine index with
  `FusionError::UnsupportedMetric` (→ `FAILED_PRECONDITION`). Euclidean returns negative
  squared-L2 in `(-∞,0]` (every candidate would clamp to 0, nullifying the term) and
  DotProduct is unbounded in `(-∞,∞)` (genuinely ambiguous to normalize), so clamping
  them would silently distort the ranking rather than normalize it. Metric-aware
  normalization for other metrics is a deliberate follow-up.
- `c` = `provenance.confidence()` when present, else the policy's `neutral_confidence`
  (AC5); the breakdown records `confidence_defaulted`.
- `r = exp(-ln2 · age / half_life)`, `age = max(0, reference_now − valid_from)` in
  seconds. `r ∈ (0,1]`; more recent ⇒ higher; future-dated ⇒ `r = 1`.

**Fused score** — normalized weighted arithmetic mean:

```
fused = (w_s·s + w_c·c + w_r·r) / (w_s + w_c + w_r)
```

with `w_s, w_c, w_r ≥ 0` and `w_s + w_c + w_r > 0` (enforced at construction).

- **Monotonicity:** `∂fused/∂x = w_x / Σw ≥ 0` for each `x ∈ {s,c,r}` — raising any one
  component (weights fixed) never lowers the fused score. (Property test.)
- **Determinism:** `fuse()` is a pure function of `(s, c, r, policy)`; `r` is a pure
  function of `(reference_now, valid_from, half_life)`. `reference_now` is supplied
  explicitly (the `AS OF` coordinate when set, else the request's single captured `now`),
  so replays/pins (#3370) are deterministic (AC6).

**Breakdown returned per result** (`FusionBreakdown`): `{ similarity, confidence,
confidence_defaulted, recency, fused }`.

---

## 7. Feature flag & graduation (ADR-0050)

New **experimental** cohort flag `semantic-retrieval-fusion = []` (empty deps — fusion
needs only always-compiled `core::provenance` + the vector path). Gated code:
`src/db/fusion.rs` (the whole module), the `fusion` field + builder on `SimilarityQuery`,
and the `similarity_search_fused` entry point. `just check-features` gains a standalone
compile line; `mcp-server` must still compile with the flag **off** (no-op path).

A lightweight `fuse()`-core micro-bench ships now (`benches/fusion_scoring.rs`, gated on
`semantic-retrieval-fusion`) to keep the per-candidate scoring hot loop honest. The full
end-to-end latency gate below is **not** measurable in-tree yet — it depends on the #3366
eval harness (a seeded 1M-vector adversarial corpus), which does not exist in this repo —
so it rides with that harness as a graduation gate, not a this-PR check.

**Graduation checklist (into `semantic-search`):**
- [ ] #3366 eval harness: fused retrieval improves grounding precision@10 by ≥ 25%
      absolute over pure-similarity on the seeded adversarial set; baseline published.
- [ ] Latency: fused k-NN (k=10, 1M vectors, provenance on 100%) p99 < 20 ms
      (rides with the #3366 eval harness; the in-tree `fusion_scoring` bench only
      covers the `fuse()`-core micro-cost, not end-to-end k-NN).
- [ ] Explainability: 100% of results carry a complete breakdown (CI-checked).
- [ ] MCP surface (`fusion_policy` on `find_similar` / `hybrid_query`) landed & documented.
- [ ] Move the `#[cfg(feature = "semantic-retrieval-fusion")]` gates to `semantic-search`.

---

## 8. MCP surface — DESIGNED HERE, DEFERRED to a follow-up PR

> **Scope note:** the namespaces lane is concurrently rewriting scoping inside
> `handle_find_similar` / `handle_hybrid_query`. To avoid a merge collision, the MCP
> `fusion_policy` wiring is **deferred to a small follow-up PR** after their changefeed
> PR merges. It is fully specified here so the follow-up is mechanical.

**Additive, optional, flag-gated** — omitting it reproduces today's behavior.

Add one optional object param `fusion_policy` to the `inputSchema.properties` of the
existing `find_similar` and `hybrid_query` tools (no new tools):

```jsonc
"fusion_policy": {
  "type": "object",
  "properties": {
    "w_similarity":         { "type": "number", "minimum": 0 },
    "w_confidence":         { "type": "number", "minimum": 0 },
    "w_recency":            { "type": "number", "minimum": 0 },
    "neutral_confidence":   { "type": "number", "minimum": 0, "maximum": 1 },
    "recency_half_life_secs": { "type": "number", "exclusiveMinimum": 0 }
  }
}
```

Handler wiring (per surface): parse the policy under
`#[cfg(feature = "semantic-retrieval-fusion")]` (mirroring `parse_provenance_filter`);
when present, force the over-fetch to the **`k`-scaled** `fused_horizon(k)` =
`min(max(3·k, 1000), FUSION_MAX_HORIZON)` (**not** a fixed `MAX_VECTOR_K + 1`: a
`k`-independent horizon degenerates fusion into a post-hoc re-sort once `k` approaches
the horizon — the HIGH-1 defect; `FUSION_MAX_HORIZON = 10_000` bounds the pool and makes
the AC4 rank-ceiling explicit),
build each candidate's `NodeResponse` (already carries `provenance` + `temporal`),
compute `fuse()` per candidate, sort by fused desc, page, and attach a `score_breakdown`
sub-object to each `SimilarityResult` / `HybridQueryResult`. Invalid weights →
`INVALID_ARGUMENT` (#3234, `retriable:false`). On `hybrid_query`, the AS OF path resolves
confidence/recency at the coordinate (via `get_node_at_time` →
`lookup_node_read_metadata`) so AC6 composes; `find_similar` has no temporal params today
and none are added (AS OF fusion is satisfied on `hybrid_query` — a documented v1 limit).

---

## 9. ACs → test-case map

| AC | Description | Proof (this PR unless noted) |
|----|-------------|------------------------------|
| AC1 | Optional fusion policy combining sim + confidence + recency, tunable weights; documented, deterministic, monotone | `SimilarityQuery::fusion` + `similarity_search_fused`; §6. MCP surface designed §8, deferred. |
| AC2 | Omitting the policy reproduces today's behavior byte-for-byte | Legacy `similarity_search` untouched; unit test `omitting_policy_is_unchanged` |
| AC3 | Per-result score breakdown (never opaque) | `FusionBreakdown`; test `breakdown_is_complete_and_non_opaque` |
| AC4 | Returned top-k are the *true* fused top-k, not a re-sort of a similarity shortlist | Rust-API adversarial fixture `tests/provenance_weighted_retrieval.rs`: target below position 3k by similarity still in fused top-k |
| AC5 | Missing provenance ⇒ configurable neutral confidence, never dropped | **Neutral-default half implemented + tested here** (`missing_provenance_uses_neutral_not_dropped`). The #3348 **filter-then-fuse** composition (hard-filter first, fuse survivors) is an MCP-layer feature (no Rust-level `ProvenanceFilter` to wire) and rides with the deferred MCP surface (§8). |
| AC6 | Fusion composes with temporal coordinates | **Reference-now recency decay proven in the core** (`fuse()`/`recency()` take an explicit `reference_now`; test `recency_uses_supplied_reference_now`). Full AS-OF **metadata** resolution (confidence/recency at a past coordinate) rides with the deferred MCP/hybrid surface; until then the `at_time` + fusion combination is **rejected** (`FusionError::AsOfNotSupported`, test `fused_search_with_at_time_is_rejected`), never silently mixing current-version metadata with a past candidate set. |
| AC7 | Invalid params (negative / all-zero / NaN weights, bad neutral / half-life) ⇒ `INVALID_ARGUMENT` | `FusionPolicyError`; tests `invalid_*_rejected`. (MCP mapping in the follow-up.) |
| AC8 | Experimental cohort flag + graduation checklist | `semantic-retrieval-fusion`, §7; `just check-features` |

Additional risk tests: monotonicity property, determinism, NaN-similarity total order,
future-dated `valid_from` clamp.

## 10. Out of scope

Trust propagation through lineage (#3371); learned ranking; hard provenance filtering
(#3348, shipped, composes); query-language fusion surface (#3354); cross-encoder
re-ranking; per-source trust registries; the #3366 eval harness (graduation dependency).
