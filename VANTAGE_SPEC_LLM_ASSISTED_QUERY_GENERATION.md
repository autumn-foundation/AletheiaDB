# 🔭 Vantage: Spec for LLM-assisted query generation

## 👤 User Story
**As a** Data Scientist or Analyst,
**I want** to write natural language questions,
**so that** the system automatically translates them into valid AQL or Cypher temporal queries without me needing to learn the underlying syntax.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
While AletheiaDB provides powerful temporal and semantic querying capabilities via AQL and Cypher, these require technical expertise and familiarity with the schema to utilize. This feature lowers the barrier to entry, allowing non-technical business users or analysts to quickly gain deep insights. It drastically reduces time-to-insight and reliance on specialized graph engineers to craft queries.

**Success Metric Definition:**
- **Translation Accuracy:** >90% of supported natural language questions are correctly translated into valid AQL/Cypher queries and execute successfully on the first attempt.
- **Latency:** The natural language translation overhead completes in <500ms (p99).

## ✅ Acceptance Criteria
- Must expose an API endpoint (and an MCP tool) that accepts a natural language string and returns the resulting executed query rows.
- Must provide the database schema and available labels/properties as context to the LLM to ensure schema-aware translation.
- Must support temporal phrasing (e.g., "Who knew Alice in 2023?" or "Show me users similar to this article over the last month").
- Must gracefully fall back with an actionable error message if the query cannot be safely generated or executed (e.g., "Could not determine temporal context. Please specify a time range.").

## 🚫 Out of Scope
- Multi-turn conversational agents with stateful memory (Phase 2).
- Automatic execution of DML/mutating queries (only read-only queries should be generated for safety).
