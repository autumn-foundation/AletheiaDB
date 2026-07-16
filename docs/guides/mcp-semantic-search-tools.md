# MCP Semantic-Search Analysis Tools (Issue #2907)

Six read-only MCP tools expose the **stable `semantic-search` cohort's**
analysis primitives to LLM agents. They let an agent reason over the *shape* of
the embedding space — paths, analogies, centroids, near-duplicates, semantic
boundaries, and disentangled context — without leaving the MCP surface.

## Availability (Design A)

All six tools are **advertised and dispatched unconditionally**, so they always
appear in `tools/list`. Their bodies require the `semantic-search` feature to be
compiled in. On a build without it, every tool returns a structured
`FAILED_PRECONDITION` error:

```json
{
  "error": {
    "code": "FAILED_PRECONDITION",
    "message": "Tool 'semantic_path' requires the `semantic-search` feature, which is not compiled into this build. Rebuild AletheiaDB with `--features semantic-search` to enable it.",
    "retriable": false,
    "details": { "tool": "semantic_path", "required_feature": "semantic-search" }
  }
}
```

Enable with:

```bash
cargo run --bin aletheia-mcp --features mcp-server,semantic-search
```

## Tool → module map

| MCP tool | `semantic_search` module | Primitive |
|----------|--------------------------|-----------|
| `semantic_path` | `semantic_navigator::SemanticNavigator` | Vector-similarity-guided (bounded A*) path between two nodes |
| `concept_analogy` | `concept_algebra::ConceptAlgebra` | Vector analogy `a : b :: c : ?` |
| `concept_mean` | `concept_algebra::ConceptAlgebra` | Nearest nodes to the centroid of a set |
| `find_duplicate_candidates` | `highlander::HighlanderDetector` | Near-duplicate entity resolution |
| `semantic_horizon` | `horizon::HorizonEngine` | Semantic event-horizon (interior vs. boundary) |
| `context_aspects` | `chameleon::Chameleon` | Disentangle a neighbourhood into weighted aspects |

All six are **read-only** (`reader`-class in the RBAC matrix) and **budgetable**
(Issue #3353 `max_response_tokens` / `max_response_bytes` /
`priority_properties`). The four score-ranked / atomic tools (`semantic_path`,
`find_duplicate_candidates`, `concept_analogy`, `concept_mean`) never drop or
reorder results to meet a budget — only per-item payloads degrade.

## Shared conventions

- Every tool requires a **vector index** on `property_name`
  (`enable_vector_index` first); a missing index returns `FAILED_PRECONDITION`.
- Node ids are validated with `NodeId::new` — an out-of-range id returns
  `INVALID_ARGUMENT`.
- `threshold` parameters are validated to be in `[0, 1]`.
- `k` / `limit` are clamped to `[1, 1000]`; `max_depth` is clamped to `[1, 20]`.
- Centroid vectors (in `context_aspects`) are **elided by default** (Issue
  #3220) — pass `include_vectors: true` for the full float array.

## Bounds and the three flagged v1 caveats

1. **`semantic_path` is expansion-bounded.** The underlying A* search is
   otherwise unbounded (a DoS risk with attacker-controlled endpoints). The MCP
   handler derives a node-expansion budget (`max_depth * 1000`, both clamped)
   and calls the new non-breaking `SemanticNavigator::find_path_bounded(start,
   end, property, max_expansions)` overload. The unbounded `find_path` is
   retained for embedded callers.
2. **`find_duplicate_candidates` uses the node's own indexed embedding (v1).**
   The underlying `SimilarityQuery` has no per-property selector, so
   `property_name` selects the vector index whose existence is **validated**;
   the actual search resolves against the target node's indexed embedding. A
   per-property selector is a documented follow-up.
3. **`context_aspects` binds to `Chameleon` (stable), not `GraphContext`
   (experimental).**

## Agent example

An agent resolving "which documents are near-duplicates of doc 42, and what are
the distinct facets of its neighbourhood?":

```jsonc
// 1. Find near-duplicate candidates of node 42.
{ "name": "find_duplicate_candidates",
  "arguments": { "node_id": 42, "property_name": "embedding", "threshold": 0.92, "limit": 5 } }
// → { "candidates": [ { "node_id": 87, "similarity": 0.96 } ], "count": 1, "threshold": 0.92, "target": 42 }

// 2. Decompose node 42's neighbourhood into semantic aspects.
{ "name": "context_aspects",
  "arguments": { "node_id": 42, "property_name": "embedding", "k": 3 } }
// → { "node_id": 42, "count": 3,
//     "aspects": [ { "weight": 0.5, "exemplars": [10, 11], "centroid": { "type": "vector", "dim": 384, "elided": true } }, ... ] }

// 3. Trace a similarity-guided path from 42 to a target concept 99.
{ "name": "semantic_path",
  "arguments": { "start": 42, "end": 99, "property_name": "embedding", "max_depth": 6 } }
// → { "path": [42, 55, 99], "length": 3, "start": 42, "end": 99, "property_name": "embedding" }
```

## HTTP projection

Each tool is also served by the unified `aletheia-server` crate under
`/semantic/*` (`/semantic/path`, `/semantic/analogy`, `/semantic/mean`,
`/semantic/duplicates`, `/semantic/horizon`, `/semantic/aspects`), forwarding
the same request through the shared MCP dispatch under the standard read-class
authorization and resource-limit guards, so HTTP and MCP responses are
byte-identical.
