# ADR-0050: Experimental Feature Categorization & Graduation Pattern

## Status

Accepted (2026-04-18, 0.1 release)

## Context

The `experimental` ("Nova") R&D playground had grown to **54 modules**
(~20,700 LOC) all gated by a single empty `nova = []` feature flag. This
created several problems as the project approached 0.1:

1. **All-or-nothing**: enabling `nova` compiled every experiment from
   probabilistic reasoning to Mermaid export, dragging in the full surface area
   even when the caller only wanted associative retrieval.
2. **No graduation path**: there was no precedent or convention for moving a
   stable module out of the playground without breaking back-compat or
   inventing the pattern ad hoc each time.
3. **Lost taxonomy**: the modules span very different use cases — search,
   prediction, anomaly detection, visualisation — but the flat module list
   buried that structure.
4. **Stable-enough cohort gated as experimental**: the semantic-search cohort
   (Fishing, Gestalt, Cartographer, Highlander, …) had been usable since the
   `usearch` integration landed and could fairly be promoted to a stable
   feature.

## Decision

Split the monolithic `nova` flag into **five category feature flags**, graduate
the semantic-search cohort to a stable top-level feature, and reserve `nova` as
an umbrella for genuine R&D only.

### Feature flag taxonomy

| Flag | Status | Cohort |
|------|--------|--------|
| `semantic-search` | Stable | Retrieval, matching, clustering, traversal, entity resolution |
| `semantic-reasoning` | Experimental | Prediction, synthesis, counterfactual simulation |
| `semantic-temporal` | Experimental | Bi-temporal + semantic analysis |
| `semantic-diagnostics` | Experimental | Anomaly detection, validation, health monitoring |
| `semantic-characterization` | Experimental | Concept characterization + LLM/visualization export |
| `nova` | Umbrella | Enables all four `semantic-*` cohorts (but **not** `semantic-search`) |

### Cargo.toml dependencies

Two cross-category code dependencies require explicit Cargo feature deps so a
single category flag still compiles standalone:

- `semantic-reasoning = ["semantic-diagnostics"]` — `alchemy` imports `wormhole`.
- `semantic-characterization = ["semantic-temporal"]` — `graph_context` imports
  `temporal_narrative`.

### Directory layout

```
src/
├── semantic_search/                  # Stable, top-level
│   └── { fishing, gestalt, cartographer, ... }.rs
└── experimental/
    ├── mod.rs                        # `pub use <category>::*` re-exports
    ├── reasoning/
    ├── temporal/
    ├── diagnostics/
    └── characterization/
```

`experimental/mod.rs` glob-re-exports each category submodule, preserving the
original `aletheiadb::experimental::sherlock::Sherlock` paths so existing
callers don't break (as long as they enable the matching category flag).

### Graduation pattern (template for future categories)

Because the experimental categories share the stable `semantic-*` flag prefix,
graduation is almost entirely a documentation + file-move change — no flag
rename, no churn for callers already opting into a single cohort.

When an experimental cohort reaches stable quality:

1. Remove the cohort from the `nova` umbrella's dependency list in `Cargo.toml`.
2. `git mv` the modules from `src/experimental/<category>/` to a top-level
   `src/<cohort>/` directory.
3. Replace the gated `mod <category>; pub use <category>::*;` in
   `src/experimental/mod.rs` with a top-level
   `#[cfg(feature = "<flag>")] pub mod <cohort>;` in `src/lib.rs`.
4. Update doc-comment paths in the moved modules (search-cohort example:
   `aletheiadb::experimental::fishing` → `aletheiadb::semantic_search::fishing`).
5. Document the graduation and any path changes in `CHANGELOG.md`.

The feature flag name stays the same — a caller already opting into
`semantic-reasoning` today keeps working after graduation; only the `nova`
umbrella shrinks.

Reference implementation: this ADR's accompanying PR (semantic-search graduation).

### Stub modules

Three modules (`mosaic`, `paradox`, `wildfire`) had files in `src/experimental/`
but were **never declared** in `src/experimental/mod.rs` and contain code that
does not compile against the current internal API. They have been moved into
the appropriate category directory (search, diagnostics, characterization
respectively) but their `pub mod` declarations are commented out. Reviving them
is tracked as follow-up work.

## Consequences

### Positive

- **Granular opt-in**: a caller wanting only Fishing pays for one cohort, not 54.
- **Clear graduation path**: the search-cohort migration documents the
  template; future graduations follow the same recipe.
- **Path stability for experimental modules**: the `pub use` re-export trick
  means existing `aletheiadb::experimental::<module>` paths keep working.
- **Discoverability**: category names map directly to use cases in the README
  and ADRs.

### Negative / breaking

- **0.1 breaking change**: the `nova` umbrella no longer pulls in the
  semantic-search cohort. Callers must add `"semantic-search"` to their
  `features` list to retain previous behaviour. Documented in
  `CHANGELOG.md`.
- **Path change for graduated modules**: `aletheiadb::experimental::fishing` →
  `aletheiadb::semantic_search::fishing`. Same for the other 13 search
  modules.
- **Internal cross-deps codified**: two pairs of categories now have implicit
  feature dependencies. If a future module introduces a new cross-category
  import, the dependency graph in `Cargo.toml` must be updated.

### Verification

- `just check-features` compiles each category flag in isolation.
- `cargo test --features semantic-reasoning --test nova_oracle` exercises Oracle.
- `cargo run --features semantic-temporal --example story_demo` exercises
  Temporal Narrative.
- `cargo run --features semantic-search,semantic-reasoning --example russian_writers`
  exercises a mixed-category flow.

## References

- [ADR-0049 Muse: Semantic Ideation](0049-muse-semantic-ideation.md) — prior
  experimental-module ADR that this work builds on.
- `src/mcp/mod.rs` and `src/embeddings/mod.rs` — established feature-gated
  module patterns this graduation follows.
