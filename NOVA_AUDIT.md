# Nova Audit Report

This document audits the experimental modules in `src/experimental` (the "Nova" features). It classifies each module by its utility, stability, and recommended future path.

## Classification Legend

*   **🟢 Core Candidate:** High utility, stable, aligned with core value proposition. Should graduate to main crate.
*   **🟡 Incubating:** Interesting feature, potential utility, needs more work or user feedback. Keep in `experimental` but maintain.
*   **🔴 Dead/Toy:** Low utility, redundant, or out of scope (e.g. client-side visualization). Candidate for removal or externalization.

## Module Inventory

| Module | Purpose | Status | Recommendation | Notes |
| :--- | :--- | :--- | :--- | :--- |
| **Alchemy** | Semantic graph transformation (crystallize edges from similarity). | 🟡 Incubating | Keep | Powerful for KG construction/refinement. |
| **Ariadne** | Semantic thread weaving (pathfinding via edges + vectors). | 🟡 Incubating | Keep | Useful for narrative generation and causal tracing. |
| **Cartographer**| Semantic clustering and reification (K-Means -> Nodes). | 🟡 Incubating | Keep | Good for auto-categorization. |
| **Chameleon** | Context-aware faceted search (neighborhood clustering). | 🟡 Incubating | Keep | Advanced search feature. |
| **Chimera** | Entity synthesis (merge two nodes). | 🟡 Incubating | Keep | Useful for "what-if" scenarios. |
| **Chronos** | Temporal pathfinding & analysis. | 🟢 Core Candidate | **Graduate** | Critical for temporal graph analysis. Move to `src/query/temporal`. |
| **Concept Algebra** | Vector arithmetic (add/sub/analogy) on nodes. | 🟢 Core Candidate | **Graduate** | Basic vector utility. Move to `src/index/vector/ops`. |
| **Dissonance** | Semantic outlier detection (topology vs vector mismatch). | 🟡 Incubating | Keep | Good for data quality/anomaly detection. |
| **Dreamer** | Semantic trajectory extrapolation (predict future vector). | 🟡 Incubating | Keep | Unique temporal-vector feature. |
| **Echo** | Temporal pattern matching (activity histograms). | 🔴 Dead | **Deprecate** | Niche, complex, overlaps with other temporal analytics. |
| **Fishing** | Associative retrieval (vector + graph expansion). | 🟢 Core Candidate | **Graduate** | This is essentially "Hybrid Search". Core feature. |
| **Gestalt** | Semantic subgraph matching (pattern matching with vectors). | 🟢 Core Candidate | **Graduate** | Powerful query primitive. |
| **Gravity** | Semantic influence analysis (mass/orbit). | 🔴 Dead | **Deprecate** | Interesting metaphor but complex and niche utility. |
| **Highlander** | Entity resolution (deduplication). | 🟢 Core Candidate | **Graduate** | Essential for data quality. |
| **Hindsight** | Counterfactual analysis (simulation overlay). | 🟡 Incubating | Keep | Very powerful for reasoning/planning. |
| **Janus** | Semantic bridge detection (connects clusters). | 🟡 Incubating | Keep | Good for structural analysis. |
| **Kairos** | Semantic event detection (timeline generation). | 🟢 Core Candidate | **Graduate** | Useful for "Memory" and LLM context. |
| **Kaleidoscope**| Force-directed layout engine (2D visualization). | 🔴 Dead | **Remove** | Out of scope for backend DB. Should be a client tool. |
| **Metaphor** | Semantic graph alignment (subgraph isomorphism + vectors). | 🟡 Incubating | Keep | Advanced reasoning feature. |
| **Mnemosyne** | Memory consolidation (keyframe extraction). | 🟢 Core Candidate | **Graduate** | Critical for LLM memory management. |
| **Muse** | Semantic ideation (finding gaps in vector space). | 🟡 Incubating | Keep | Creative AI feature. |
| **Oracle** | Probabilistic reasoning (PageRank, Reachability). | 🟡 Incubating | Keep | Graph algorithms are useful but maybe distinct from core. |
| **Prism** | Vector decomposition (explainability). | 🟡 Incubating | Keep | Good for explainable AI. |
| **Prophet** | Link prediction (Adamic-Adar + Vectors). | 🟢 Core Candidate | **Graduate** | Standard graph ML feature. |
| **Ripple** | Causal causality detection (lagged correlation). | 🟡 Incubating | Keep | Temporal reasoning feature. |
| **Semantic Navigator** | A* pathfinding on semantic vectors. | 🟢 Core Candidate | **Graduate** | Core navigation primitive. |
| **Sentinel** | Semantic validation (rules, constraints). | 🟢 Core Candidate | **Graduate** | "Semantic Schema". Essential for production. |
| **Sherlock** | Temporal pattern matching (sequences). | 🟢 Core Candidate | **Graduate** | Core temporal query feature. |
| **Sybil** | Memetic propagation simulation. | 🔴 Dead | **Deprecate** | Simulation is niche. |
| **Synapse** | Adaptive pathfinding (Hebbian learning). | 🟡 Incubating | Keep | Interesting self-optimizing index. |
| **Telepathy** | Spreading activation. | 🟡 Incubating | Keep | Standard AI technique. |
| **Temporal Diff** | Diffing graph states. | 🟢 Core Candidate | **Graduate** | Essential for version control/sync. |
| **Temporal Narrative** | Natural language history generation. | 🟡 Incubating | Keep | Good for LLM output, but maybe application layer. |
| **Thermos** | Semantic volatility gauge. | 🟡 Incubating | Keep | Good metric. |
| **Wormhole** | Semantic shortcut detection (link prediction). | 🟡 Incubating | Keep | Variant of link prediction. |

## Action Plan

1.  **Mark Deprecated:** `Echo`, `Gravity`, `Kaleidoscope`, `Sybil`. These will be marked with `#[deprecated]` and eventually removed.
2.  **Stabilize Incubating:** Ensure all "Incubating" modules have basic tests and docs.
3.  **Graduate Core Candidates:** Plan to move these to core modules in future PRs.

## Immediate Changes (This PR)

*   Update `src/experimental/mod.rs` to reflect these statuses in documentation.
*   Add `#[deprecated]` to the "Dead" modules.
