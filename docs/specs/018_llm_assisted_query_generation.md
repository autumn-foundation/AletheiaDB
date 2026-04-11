# 🔭 Vantage Spec: LLM-Assisted Query Generation

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-018 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/api/llm/` (Proposed) |

## 1. 👤 User Stories

> **As a** Data Scientist, Business Analyst, or non-technical user,
> **I want to** write natural language questions,
> **So that** the system automatically translates them into valid AQL or Cypher temporal queries without me needing to learn the underlying syntax.

## 2. 🧐 The "So What?" (Business Value)

While AletheiaDB provides powerful temporal and semantic querying capabilities via AQL and Cypher, these require technical expertise and familiarity with the schema to utilize.

**The Gap:**
- **Steep Learning Curve:** Users must learn AQL syntax and the exact schema.
- **Bottleneck:** Non-technical users must rely on data engineers to write and execute queries for them.

**ROI:**
- **Democratized Data Access:** Lowers the barrier to entry, allowing non-technical business users or analysts to quickly gain deep insights.
- **Velocity:** Drastically reduces time-to-insight and reliance on specialized graph engineers to craft queries.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **NL-to-AQL Endpoint**:
    -   Must expose an API endpoint (and an MCP tool) that accepts a natural language string and returns the resulting executed query rows.
2.  **Schema Awareness**:
    -   Must provide the database schema and available labels/properties as context to the LLM to ensure schema-aware translation.
3.  **Temporal & Semantic Support**:
    -   Must support temporal phrasing (e.g., "Who knew Alice in 2023?" or "Show me users similar to this article over the last month").
4.  **Error Handling**:
    -   Must gracefully fall back with an actionable error message if the query cannot be safely generated or executed (e.g., "Could not determine temporal context. Please specify a time range.").

### Non-Functional Requirements
-   **Metric Definition:**
    -   **Translation Accuracy:** >90% of supported natural language questions are correctly translated into valid AQL/Cypher queries and execute successfully on the first attempt.
    -   **Latency:** The natural language translation overhead completes in <500ms (p99).

## 4. 🚫 Out of Scope (Phase 1)

-   **Conversational Memory**: Multi-turn conversational agents with stateful memory (Phase 2).
-   **Write Operations**: Automatic execution of DML/mutating queries (only read-only queries should be generated for safety).
