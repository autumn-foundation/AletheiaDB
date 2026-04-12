# 🔭 Vantage: Spec for Gestalt (Semantic Subgraph Matching)

## 👤 User Story
**As a** Fraud Analyst or Threat Hunter,
**I want** to find "fuzzy" structural patterns in a transaction graph,
**so that** I can detect money laundering rings or coordinated attack vectors even if they slightly vary their connection structure or transaction amounts over time.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Current graph databases rely on rigid subgraph matching (e.g., Cypher MATCH). If a bad actor adds an intermediary account (an extra node) or changes the transaction slightly, the rigid query fails. Gestalt solves the "fragile query" problem by matching subgraphs based on *semantic* similarity and *structural* approximation, leading to a higher detection rate of mutated fraud rings and lowering the time analysts spend tweaking queries.

**Success Metric Definition:**
- **Match Recall:** Gestalt identifies >90% of subgraphs where node embeddings deviate by up to 10% from the baseline pattern.
- **Query Latency:** Subgraph pattern matching with up to 5 nodes/edges completes in <50ms for a graph of 1M nodes.

## ✅ Acceptance Criteria
- Must define a Pattern Subgraph specification containing Node Constraints (Label, Vector Similarity Threshold) and Edge Constraints (Label).
- Must return a list of concrete subgraphs (nodes and edges) from the database that meet or exceed the similarity threshold of the pattern.
- Must handle missing semantic data (e.g., missing vectors) gracefully without panicking, simply excluding those nodes from potential matches.
- Must provide a cumulative "Gestalt Score" (0.0 to 1.0) for each matched subgraph representing the overall confidence of the match.

## 🚫 Out of Scope
- Real-time continuous pattern detection on streaming data (Phase 2).
- Cross-shard distributed pattern matching (Phase 2).
- Dynamic edge weight considerations (Only semantic vectors on nodes will be scored in MVP).
- Auto-generating patterns from historical examples (This is "Muse" or "Alchemy", not Gestalt).
