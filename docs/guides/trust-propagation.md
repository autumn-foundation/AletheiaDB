# Trust Propagation: Computed Confidence over Lineage (Issue #3382)

Write-time [provenance](../../src/core/provenance.rs) confidence (Issue #3224)
stops at the first hop: a fact written directly from a source carries that
source's confidence, but the facts agents actually consume are **derived** — a
summary distilled from ten documents, an entity merged from three records, an
inference chained across prior inferences. Today a derived fact's confidence is
whatever its writer typed (invented, stale, or missing), and it never updates
when the evidence moves.

**Trust propagation computes over
[derivation lineage](derivation-lineage.md) (Issue #3371):** a derived fact
carries a **computed** confidence, inferred from its upstream evidence through a
declared, deterministic combination policy, with an explainable per-fact
breakdown and recomputation when the evidence changes. The writer-declared value
is kept **distinct** and is never overwritten.

> **Feature gate:** the whole feature lives behind the experimental
> `semantic-reasoning` ("Nova") cohort flag. It is compiled out entirely when
> the flag is off — zero write-path/read-path overhead — and never referenced
> from the write path or any non-trust read path. This wave is **Rust-API-only**;
> the MCP `trust_breakdown` surface is a designed, deferred follow-up.

## Declared vs computed — never conflated

`computed_confidence(reference)` returns a `ComputedConfidence`:

| Field | Meaning |
|-------|---------|
| `declared: Option<f64>` | The writer-asserted `provenance.confidence()`, **untouched**. `None` when none was written. |
| `computed: f64` | The value derived from upstream evidence under the active policy. |
| `has_lineage: bool` | `false` ⇒ the fact had no lineage, so `computed` simply equals its own declared/leaf value (nothing combined). |
| `combinator: Option<TrustCombinator>` | Which combinator produced `computed` (`None` for a leaf). |
| `has_missing_inputs: bool` | A contributing input had no confidence and a rule was applied (always flagged). |
| `has_retracted_inputs: bool` | A contributing input was retracted or absent (dominated toward `0.0`). |
| `truncated: bool` | The scalar walk hit the hard depth backstop (defence-in-depth; not normally reached). |

Computation is **lazy — never stored**. Every read recomputes from current
state, so a superseding write or an [Issue #3230](mcp-query-tool.md) retraction
of an upstream fact flows downstream **for free** (the staleness bound is zero),
and recorded history is never mutated.

## The two combinators

Given the set of *contributing* upstream confidences `c_1..c_n ∈ [0,1]` at a
derivation node:

| Combinator | Formula | Intuition |
|------------|---------|-----------|
| `WeakestLink` | `min(c_1..c_n)` | Conservative: a chain is only as strong as its weakest evidence. |
| `NoisyOr` | `1 − ∏_i (1 − c_i)` | Independence/corroboration: more supporting facts raise confidence. |

Both are deterministic, order-independent, and hand-computable. Worked example
for a node over `{0.9, 0.9, 0.3}`:

- weakest-link → `min = 0.3`.
- noisy-OR → `1 − (0.1)(0.1)(0.7) = 0.993`.

These are explicit approximations that treat evidence as **independent** per
policy — not full possible-worlds probabilistic semantics. Over a diamond DAG a
shared ancestor's value is computed once (memoized) but its influence is counted
in **each** parent under noisy-OR (the documented independence approximation), so
`noisy_or{b=0.6, c=0.6}` from a shared `r=0.6` is `0.84`, not `0.6`.

## Per-label policy + durable sidecar

A `TrustPolicy` declares the active `combinator` and the
`MissingConfidencePolicy`. Resolution per fact is **most-specific-wins**: a
per-label / per-edge-type override, else the database default. The `Default` is
the conservative `WeakestLink` + `Zero`.

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::core::lineage::LineageRef;
use aletheiadb::experimental::reasoning::trust_propagation::{
    ComputedConfidenceFilter, MissingConfidencePolicy, TrustOptions, TrustPolicy,
};

let db = AletheiaDB::new()?;

// Database-wide default.
db.set_trust_policy(TrustPolicy::noisy_or(MissingConfidencePolicy::Zero))?;

// Per-label override (wins for facts with this label / edge-type).
db.set_trust_policy_for_label("Merge", TrustPolicy::weakest_link(MissingConfidencePolicy::Ignore))?;

// Discover the active policies (label-sorted).
let view = db.list_trust_policies();

// Read computed confidence.
let cc = db.computed_confidence(reference)?;    // as of now
let cc_then = db.computed_confidence_as_of(reference, t)?; // as recorded at tx-time t
// Or resolve the current version of an entity directly:
let cc_node = db.node_computed_confidence(node_id)?;
```

Policies are held in a `TrustRegistry` that is **entirely off the data write
path** (a leaf, like the snapshot registry). It is durably persisted to
`{data_dir}/trust_policy.json` when index persistence is enabled (atomic
temp→fsync→rename→parent-fsync; a corrupt/future-version sidecar is quarantined
to `*.corrupt` and startup proceeds with an empty registry — it never bricks
startup), and in-memory-only for the ephemeral `AletheiaDB::new()`. A policy set
**after** the facts already exist is honored on the **next** read — nothing is
stored, so evaluation always reflects the current policy.

## Explainability: `trust_breakdown`

`trust_breakdown(reference, &TrustOptions)` returns the computation tree: each
upstream fact's `reference`, `status`, resulting `confidence`, its `source`
classification, the `combinator` applied at that node, and its `children` —
bounded by `max_depth` and `max_nodes` with a `truncated` signal.

```rust
let bd = db.trust_breakdown(reference, &TrustOptions::new().with_max_depth(4));
```

- **Truncation is honest:** the caps govern how many nodes are *serialized*,
  never how many confidences are *combined*. A truncated node still reports its
  **full-accuracy** `confidence` (the breakdown reads from a single shared memo
  computed for the whole closure — an O(n) build).
- **`max_nodes` counts descendants:** the root is always emitted and does not
  count, so `max_nodes = N` serializes up to `root + N` nodes.

## Retraction & missing-confidence rules

Per-upstream contribution:

| Upstream state | `ConfidenceSource` | Contribution |
|----------------|--------------------|--------------|
| Root with declared confidence | `Declared` | its confidence |
| Intermediate derivation | `Computed` | recursively computed |
| Root written without confidence | `Missing` | per `MissingConfidencePolicy` (`Zero` → 0.0 / `Neutral` → 0.5 / `Ignore` → dropped), **always flagged** |
| Valid-time retracted (ended as of now) | `Retracted` | **0.0, dominates** |
| Deleted / dangling in current state | `Absent` | **0.0, dominates** |

**Retraction/absence dominates under BOTH combinators.** When any direct
contributor at a node is `Retracted` or `Absent`, the node **short-circuits to
`0.0`** — implemented by an **explicit terminal-child check** in the node
combine step, *not* by relying on `0.0` being a noisy-OR identity term (under
noisy-OR a positional `0.0` would be absorbed by a live sibling, e.g.
`noisy_or{retracted 0.0, live 0.9} = 0.9`). The cap is **local** to the node with
the terminal contributor: the resulting `0.0` flows to the parent as an ordinary
value, while `has_retracted_inputs` bubbles up the whole subtree. `Retracted` and
`Absent` are **distinct** classifications so the explanation tells "we withdrew
this as of a valid time" apart from "this is gone".

## Bi-temporal AS OF (transaction time and valid time are independent axes)

`computed_confidence_as_of(reference, T)` evaluates the policy over lineage +
confidences **as recorded at transaction time `T`**:

- Each contributing reference's confidence is read from the entity's **head as
  recorded at `T`** (the latest version with tx-time start `<= T`), which makes
  the now-eval reactive (head = current) and the as-of eval revision-stable
  (head as recorded at `T`). The version pinned in the `LineageRef` only sets the
  reference's Current-vs-Superseded status; the confidence is not read from it.
- **No-op AS OF:** with `T` at or after the latest transaction time, the result
  equals the unscoped `computed_confidence`.

**Valid-time terminality is keyed on an explicit valid-time coordinate**
(Issue #3382). Transaction time and valid time are **independent** axes: `T`
scopes *which recorded version is visible*, while the valid-time coordinate
scopes *terminality*. A fact whose valid interval has *ended at the coordinate*
(`valid_to <= valid_now`) is `Retracted`; a fact retracted **effective-after**
the coordinate (still valid there) or holding a naturally-bounded interval that
**contains** the coordinate is **live** and contributes its confidence. The
coordinate flows through the whole recursive lineage closure, so every upstream
fact is judged terminal-or-not at the same valid time.

Supply the coordinate via
`computed_confidence_as_of_bitemporal(reference, valid_time, T)` (scalar) or
`TrustOptions::with_as_of_valid_time(valid_time)` (breakdown). **Omitting it
defaults the coordinate to wallclock `time::now()`, reproducing the prior
behavior exactly:** `computed_confidence` evaluates at `(now, now)`, and
`computed_confidence_as_of(reference, T)` evaluates valid time at `now` while
scoping transaction time to `T`.

```rust
// Evaluate terminality as it stood at an earlier valid-time coordinate:
// a fact whose interval has since ended is still live at `earlier`.
let cc = db.computed_confidence_as_of_bitemporal(reference, earlier, tt)?;

// Same axis in the explainable breakdown:
let bd = db.trust_breakdown(
    reference,
    &TrustOptions::new().with_as_of_valid_time(earlier),
);
```

A version that merely **predates** `T` (not yet recorded at `T`) is `Absent` and
contributes `0.0`, but is **not** a retraction and does not flag
`has_retracted_inputs` — this transaction-time carve-out is unaffected by the
valid-time coordinate.

## Cycle & depth safety

Cycles are impossible by construction (lineage version-space is a DAG — see the
[lineage guide](derivation-lineage.md)). Defence-in-depth adds a DFS
visited/stack guard, per-version memoization (a diamond DAG evaluates each shared
ancestor once), and a hard scalar recursion ceiling (`SCALAR_MAX_DEPTH = 1024`,
distinct from the caller-facing `max_depth` default of 32). Beyond the scalar
ceiling the value is computed conservatively from what was reached and the result
is flagged `truncated` — never over-trusted.

## Predicate & fusion (AC7)

`ComputedConfidenceFilter` (`min`/`max`, inclusive; an inactive filter matches
everything) filters reads/traversal results by computed confidence and composes
with the declared-confidence
[`ProvenanceFilter`](../../src/core/provenance.rs) — both AND together:

```rust
db.computed_confidence_matches(reference, &ComputedConfidenceFilter::at_least(0.5))?;
db.passes_confidence_filters(reference, Some(&provenance_filter), &computed_filter)?;
let kept = db.filter_by_computed_confidence(&references, &ComputedConfidenceFilter::range(0.5, 1.0))?;
```

Computed confidence is also exposed as a fusion signal for provenance-weighted
retrieval (Issue #3372, behind both feature flags); #3372 owns the fusion
mechanics — this only supplies the signal.

## Performance

Computed confidence for a fact with a ≤100-node lineage closure completes well
within the temporal-reconstruction budget; single-fact declared-confidence reads
are unaffected. The opt-in on-cost is quantified by
`benches/trust_propagation.rs` (Criterion, gated `semantic-reasoning`), sampling
`computed_confidence` and `trust_breakdown` over a fixed fixture. Zero-overhead
when the feature is disabled is guaranteed by construction and verified via
`cargo build --no-default-features`.

## See also

- [Derivation lineage](derivation-lineage.md) — the fact-to-fact structure this
  computes over (Issue #3371).
- [Design doc](../plans/2026-07-20-trust-propagation.md) — the locked design,
  test matrix, and reconciliation notes.
