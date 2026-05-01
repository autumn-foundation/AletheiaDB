# 🔭 Vantage: Spec for Multi-Index Support

## 👤 User Story
**As a** Data Architect building a multimodal knowledge graph,
**I want** to create and query multiple distinct vector indexes (e.g., text content, image representations, and code snippets) within the same database,
**so that** I can accurately search and retrieve semantically similar nodes across different data types or semantic spaces without interference.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB limits users to a single vector index per database. Real-world applications often involve multiple types of embeddings for the same or different entities (e.g., `product_description` vs. `product_image`, or different embedding models for different languages). A single vector index forces users to either mix disparate embedding spaces (which degrades search quality) or spin up multiple database instances (which adds operational overhead and breaks graph traversals). Multi-index support allows a unified view of diverse semantic spaces within a single connected graph.

**Success Metric Definition:**
- **Search Quality:** Queries against specific indexes return accurately ranked results without cross-contamination from other semantic spaces.
- **Performance:** Adding a new vector index should not degrade the query latency of existing vector indexes by more than 5%. Memory overhead scales linearly with the number of indexes.

**Gap Analysis:**
The current implementation allows only one vector index (HNSW) for the entire database. Standard vector databases (like Qdrant, Milvus) and multimodal databases inherently support multiple named collections or indexes per database.

## ✅ Acceptance Criteria
- Must provide API and AQL syntax to create, drop, and manage multiple named vector indexes.
- Must allow a node to participate in multiple indexes simultaneously, assuming it possesses the required properties.
- Hybrid queries (`SIMILAR TO`, `RANK BY SIMILARITY TO`) must be explicitly targetable to a specific named index.
- Vector insertion, update, and deletion logic must correctly route to and update the targeted named index.

## 🚫 Out of Scope
- Cross-index vector joins (e.g., retrieving nodes by computing the distance between a vector in Index A and a vector in Index B).
- Distributed index splitting across shards (Phase 6).
