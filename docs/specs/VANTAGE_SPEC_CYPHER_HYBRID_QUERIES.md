# 🔭 Vantage: Spec for Cypher Hybrid Graph+Vector Queries

## 👤 **User Story:**
As an AI Application Developer, I want to execute Cypher queries that combine semantic vector similarity search with structural graph traversal, so that I can retrieve contextually relevant nodes while strictly enforcing relational constraints (e.g., "Find documents similar to 'AI' that were authored by people in my organization").

## 📈 **The "So What?" (Business Value):**
Building AI applications typically requires separate vector databases for semantic search and graph databases for relational logic. Forcing developers to do client-side joins between these two systems causes extreme latency and complexity. By allowing hybrid queries natively in Cypher, we eliminate this friction, reducing query latency dramatically and making AletheiaDB the obvious choice for enterprise RAG (Retrieval-Augmented Generation) architectures.

**Success Metric:** Time-To-First-Result for a hybrid query < 5ms, and 99th percentile total execution < 50ms for 1M node datasets.

## ✅ **Acceptance Criteria:**
- The Cypher parser must recognize and parse vector similarity functionality (e.g., via a custom function or `CALL` procedure).
- The query planner must intelligently decide whether to perform vector search first (if highly selective) or graph traversal first.
- The execution engine must successfully combine graph pattern matching results with k-NN vector search results.
- Must allow parameterization of the input vector (i.e., binding the float array via `$embedding`).

## 🚫 **Out of Scope:**
- Automatic LLM embedding generation from raw text (clients must provide the pre-computed vector).
- Support for complex dimension reduction during the query.
- Real-time continuous streaming / subscriptions to hybrid query results.
