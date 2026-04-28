# 🔭 Vantage: Spec for SQL:2011 Temporal Syntax

👤 **User Story:**
As a Database Administrator familiar with standard relational databases, I want to use SQL:2011 standard temporal syntax (like `SYSTEM_TIME AS OF` and `FOR SYSTEM_TIME BETWEEN`), so that I can easily migrate existing temporal queries to AletheiaDB without learning a completely new proprietary query language.

✅ **Acceptance Criteria:**
- Must support the standard `FOR SYSTEM_TIME AS OF <timestamp>` clause for point-in-time queries.
- Must support the standard `FOR SYSTEM_TIME FROM <start> TO <end>` and `FOR SYSTEM_TIME BETWEEN <start> AND <end>` clauses for range queries.
- Must seamlessly integrate with the existing Cypher-like AST/IR pipeline without breaking current AQL semantics.

🚫 **Out of Scope:**
- Application-time (Valid Time) SQL:2011 syntax (only System Time / Transaction Time is targeted for Phase 1).
- Full SQL standard compliance beyond the temporal extensions.
