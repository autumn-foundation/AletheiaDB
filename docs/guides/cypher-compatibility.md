# openCypher Compatibility Matrix (Issue #561)

AletheiaDB implements a **read-only subset** of openCypher with bi-temporal and
vector extensions. This document is the authoritative, machine-verified contract
for **what that subset is** and **how out-of-subset queries fail**. Every row in
the tables below is pinned by an executable case in the compatibility suite
(`src/cypher/compat.rs`); the doc and the suite are kept in lockstep — if they
disagree, that is a bug.

The design goal is **honesty over coverage**: a construct is either *supported*
(parses/executes correctly) or *rejected with a structured error*. AletheiaDB
never silently answers an unsupported query with wrong data — with the pending
exceptions explicitly called out in [§4](#4-pending--tracked-but-not-yet-on-trunk).

For the prose companion see the "Cypher Query Language → Supported Syntax"
section of the top-level [`CLAUDE.md`](../../CLAUDE.md).

---

## How the suite works / how to run

```bash
# Run the whole compatibility suite (requires the `cypher` feature)
cargo test --features cypher --lib cypher::compat
```

The suite is table-driven. A single automated seed builder, `compat_db()`,
constructs a deterministic known graph (no external fixtures, no I/O). A curated
`CORPUS` of `Case` values then pins each construct to an expectation:

| Expectation | Entry point exercised | Assertion |
|-------------|-----------------------|-----------|
| `Executes(n)` | `AletheiaDB::execute_cypher[_with_params]` | exact returned row count `== n` |
| `ExecutesAtLeast(n)` | same | returned row count `>= n` |
| `Parses` | `cypher::parse_cypher[_with_params]` | parse + convert returns `Ok` |
| `Rejected(kind, substr)` | `cypher::parse_cypher[_with_params]` | returns `Err(CypherError::<kind>)` whose message contains `substr` |

**Why rejections assert the precise `CypherError` variant.** At the public
`execute_cypher` boundary the internal `CypherError` variants collapse into the
coarser `QueryError` family (`SyntaxError` / `UnsupportedFeature` /
`InvalidParameter`). To pin *intent* — "this is rejected *because* it is
unsupported", not merely "this errored" — the rejection cases call
`cypher::parse_cypher` / `parse_cypher_with_params` directly and match on the
exact `CypherError` variant. (Standalone `UNWIND` is planned through
`plan_cypher`, not `parse_cypher`, so UNWIND is exercised via `execute_cypher`.)

Each runner (`compat_supported_execute`, `compat_supported_parse`,
`compat_rejected`) iterates its slice of the corpus and **accumulates every
failure**, then panics once with a formatted, multi-line report naming each
failing case, its query, and expected-vs-actual — so a CI failure tells you
exactly which constructs regressed, not just the first one.

### Seed graph (`compat_db()`)

Nodes:

- `Person` **Alice** `{name:'Alice', age:30, dept:'Eng'}`
- `Person` **Bob** `{name:'Bob', age:25, dept:'Eng'}`
- `Person` **Carol** `{name:'Carol', age:40, dept:'Sales'}`
- `Person` **Dave** `{name:'Dave', age:35, dept:'Sales'}`
- `Company` **Acme** `{name:'Acme', cid:1}`
- `Company` **Globex** `{name:'Globex', cid:2}`

No node has an `email` property (used to pin openCypher-correct `IS NULL` /
`IS NOT NULL` on an absent property: `IS NULL` matches all 4 Persons,
`IS NOT NULL` matches 0 — fixed in #3511).

Edges:

- Alice `-[:KNOWS]->` Bob
- Bob `-[:KNOWS]->` Carol
- Carol `-[:KNOWS]->` Dave
- Alice `-[:WORKS_AT]->` Acme
- Dave `-[:WORKS_AT]->` Globex

---

## 1. Supported constructs

These parse **and** execute (or, where marked *convert-only*, parse + lower)
correctly.

| Construct | Example | Notes / caveats |
|-----------|---------|-----------------|
| Labeled node scan | `MATCH (n:Person) RETURN n` | |
| Unlabeled node scan | `MATCH (n) RETURN n` | scans all nodes |
| Inline property match | `MATCH (n:Person {name:'Alice'}) RETURN n` | |
| Multiple inline props | `MATCH (n:Person {dept:'Eng'}) RETURN n` | |
| `WHERE` comparisons | `WHERE n.age > 30` | `= <> > >= < <=` |
| `WHERE` boolean logic | `WHERE n.age > 30 AND n.dept = 'Sales'` | `AND` / `OR` / `NOT` |
| `WHERE ... IN` | `WHERE n.dept IN ['Eng','Sales']` | list membership |
| `STARTS WITH` / `ENDS WITH` / `CONTAINS` | `WHERE n.name STARTS WITH 'A'` | case-sensitive |
| `IS NULL` / `IS NOT NULL` | `WHERE n.email IS NULL` | openCypher-correct for both **present** and **absent** properties: a missing property *is* null, so `n.email IS NULL` matches all rows and `IS NOT NULL` matches none (absent-property inversion fixed in #3511) |
| Outgoing / incoming / undirected | `-[:KNOWS]->`, `<-[:KNOWS]-`, `-[:KNOWS]-` | |
| Relationship-type traversal | `MATCH (a)-[:KNOWS]->(b) RETURN b` | |
| Variable-length paths | `-[:KNOWS*1..3]->`, `*2`, `*`, `*2..`, `*..2` | **node-distinct / shortest-path reachability** (v1 simplification of trail semantics); open-ended bounds capped at depth 10 |
| `RETURN` / `RETURN DISTINCT` / `AS` | `RETURN DISTINCT n AS person` | `DISTINCT` dedups by entity id |
| `ORDER BY` (multi-key, `ASC`/`DESC`) | `RETURN n ORDER BY n.dept, n.age DESC` | openCypher null placement (nulls last `ASC`, first `DESC`) |
| `SKIP` / `LIMIT` | `RETURN n SKIP 1 LIMIT 2` | |
| Aggregation | `count(*)`, `count(expr)`, `count(DISTINCT e)`, `sum`/`avg`/`min`/`max`/`collect` | openCypher implicit grouping (non-aggregate `RETURN` items form the key) |
| Grouped aggregation | `RETURN n.dept, count(*)` | one row per group key |
| `WITH ... WHERE ... RETURN` | `MATCH (a:Person) WITH a WHERE a.age > 30 RETURN a` | single carried variable (optionally `AS`-aliased) |
| Chained `WITH` | `WITH a WHERE ... WITH a WHERE ... RETURN a` | |
| Standalone `UNWIND` | `UNWIND [1,2,3] AS x RETURN x` | list literal / `$param` / `null`; `RETURN x`, `DISTINCT`, `ORDER BY x`, `SKIP`, `LIMIT` |
| `OPTIONAL MATCH` (left-outer) | `MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x` | unmatched preserves base row, binds null; single pattern, unlabeled continuation node |
| `AS OF TIMESTAMP` (#550) | `MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00Z' RETURN n` | *convert-only in the suite* (avoids seeding bi-temporal history). Timestamp literal accepts ISO-8601 with `Z` **or** numeric offset (`+00:00`), date-only (midnight UTC), and Unix microseconds — all pinned. **Runtime** reconstruction is proven end-to-end in `src/cypher/tests.rs` (`test_e2e_as_of_timestamp_*`) |
| `AS OF VALID_TIME` / `AS OF SYSTEM_TIME` / `FOR VALID_TIME AS OF` (#551) | `... AS OF VALID_TIME '2024-01-15' RETURN n` | *convert-only* in the suite; runtime-proven in `tests.rs` |
| `FOR SYSTEM_TIME AS OF` (#551) | `... FOR SYSTEM_TIME AS OF '2024-01-15' RETURN n` | *convert-only*; runtime-proven (`test_e2e_for_system_time_as_of_honored_by_label_scan`) |
| Bi-temporal (#551) | `... AS OF VALID_TIME '...' AS OF SYSTEM_TIME '...' RETURN n` | *convert-only*; runtime-proven (`test_e2e_as_of_bitemporal_valid_and_system`, `test_e2e_as_of_asymmetric_bitemporal`) |
| `BETWEEN ... AND` (valid-time range, #552) | `... BETWEEN '2024-01-01' AND '2024-12-31' RETURN n` | *convert-only*; half-open `[start, end)` (start-inclusive, end-exclusive). Runtime-proven in `tests.rs` (`test_e2e_between_*`, incl. boundary inclusivity) |
| Vector similarity in `ORDER BY` | `RETURN d ORDER BY vector.similarity(d.embedding, $q) DESC LIMIT 10` | *convert-only* (needs an `Embedding` param) |
| Parameters | `MATCH (n:Person {name:$name}) RETURN n` | `$param` bindings |

---

## 2. Intentionally-unsupported constructs (rejected, never silent)

These are rejected with a **structured `CypherError`** before any wrong answer
can be produced. The suite pins the exact variant and a message substring.

| Construct | Example | Error variant | Why |
|-----------|---------|---------------|-----|
| `CREATE` | `CREATE (n) RETURN n` | `ParseError` | read-only subset; `CREATE` is not a lexer keyword, so it fails `expect(MATCH)` |
| `SET` | `MATCH (n) SET n.x = 1` | `ParseError` | mutating clause not in the read-only grammar |
| `MERGE` | `MERGE (n:Person {name:'Z'}) RETURN n` | `ParseError` | mutating clause |
| `DELETE` | `MATCH (n) DELETE n` | `ParseError` | mutating clause |
| `REMOVE` | `MATCH (n) REMOVE n.age` | `ParseError` | mutating clause |
| `DETACH DELETE` | `MATCH (n) DETACH DELETE n` | `ParseError` | mutating clause |
| `FOREACH` | `FOREACH (x IN [1] | SET ...)` | `ParseError` | mutating/procedural clause |
| `CALL` | `CALL db.labels()` | `ParseError` | procedure calls not supported |
| `LOAD CSV` | `LOAD CSV FROM 'f.csv' AS row RETURN row` | `ParseError` | data-loading clause not supported |
| Multiple plain `MATCH` clauses | `MATCH (a) MATCH (b) RETURN a` | `ParseError` | only a single `MATCH` (plus `OPTIONAL MATCH`) is accepted |
| Trailing tokens after statement | `MATCH (n) RETURN n EXTRA` | `ParseError` | "unexpected tokens after end of statement" |
| `count(DISTINCT *)` | `RETURN count(DISTINCT *)` | `ParseError` | openCypher disallows `DISTINCT *` |
| Comma-separated `OPTIONAL MATCH` | `OPTIONAL MATCH (a),(b)` | `UnsupportedFeature` | no variable-binding analysis yet; would silently produce wrong rows |
| Labeled first node of subsequent `OPTIONAL MATCH` | `MATCH (a) OPTIONAL MATCH (b:Person)-[:R]->(c) RETURN c` | `UnsupportedFeature` | continuation node must re-anchor an existing binding, not introduce a new labeled scan |
| Grouping by a whole node/edge | `MATCH (n:Person) RETURN n, count(*)` | `UnsupportedFeature` | single-entity row model can't express node-identity grouping — group by a property |
| `RETURN *` mixed with any other item | `MATCH (n:Person) RETURN *, count(*)` | `ParseError` | `*` is accepted only as the *sole* return item; mixing it (even with an aggregate) trails tokens. The converter's "RETURN * with aggregation" `UnsupportedFeature` guard is unreachable via the string parser (hand-built-AST defense only) |
| `WITH` projecting multiple items | `MATCH (a)-[:R]->(b) WITH a, b RETURN a` | `UnsupportedFeature` | positional single-variable pipeline carries one binding |
| `WITH` computed / property / aggregate projection | `MATCH (a:Person) WITH a.name AS x RETURN x` | `UnsupportedFeature` | `WITH` can only project a bound variable (optionally aliased) |
| `RETURN` of a `WITH`-dropped variable | `MATCH (a:Person) WITH a AS p WHERE p.age > 30 RETURN a` | `SemanticError` | `a` is out of scope after the `WITH` renamed/dropped it |
| Unbound parameter | `MATCH (n:Person {name:$name}) RETURN n` (no binding) | `ParameterError` | `$name` was never bound |
| `FOR SYSTEM_TIME BETWEEN` (tx-time range, #551/#552) | `... FOR SYSTEM_TIME BETWEEN '2024-01-01' AND '2024-03-31' RETURN n` | `ParseError` | **Design decision**: transaction-time RANGE scans are not exposed through Cypher; the parser requires `AS OF` after `SYSTEM_TIME`. (The tx-range context is reachable only via the `QueryBuilder` API, where the planner rejects it as an `UnsupportedFeature` — see `test_e2e_transaction_time_between_rejected`.) |
| Inverted `BETWEEN` range (#552) | `... BETWEEN '2024-12-31' AND '2024-01-01' RETURN n` | `InvalidTemporalClause` | start/end are validated (`start > end` rejected during lowering), never silently treated as empty |
| Unparseable `AS OF TIMESTAMP` literal (#550) | `... AS OF TIMESTAMP 'not-a-timestamp' RETURN n` | `InvalidTimestamp` | a malformed timestamp is rejected during lowering, never a silent `now()` fallback |

---

## 3. Known limitations (executes, with documented caveats)

- **Variable-length matching is node-distinct / shortest-path**, a deliberate v1
  simplification of openCypher **trail** (path-enumeration) semantics: each
  distinct target is bound once at its *shortest* hop-distance. A node whose
  shortest path is below `min` (or reachable only via an in-range cycle) is not
  re-emitted at a longer in-range depth. On the acyclic seed graph
  (a KNOWS chain) shortest-depth equals the only depth, so the suite's
  variable-length cases match full openCypher semantics exactly. See the
  `test_varlen_*` cases in `src/cypher/tests.rs` for the pinned simplifications.
- **Open-ended upper bounds** (`*` and `*min..`) are capped at depth 10
  (`DEFAULT_MAX_TRAVERSAL_DEPTH`).
- **`RETURN DISTINCT <scalar projection>`** deduplicates by entity id, not the
  projected value (property projection is not yet lowered into the row model).

> **Fixed (#3511):** `IS NULL` / `IS NOT NULL` on an *absent* property was
> previously inverted vs openCypher (a missing property matched `IS NOT NULL`
> instead of `IS NULL`). #3511 re-lowered property-access `x IS NULL` →
> `Or(NotExists, Eq(Null))` and `x IS NOT NULL` → `And(Exists, Ne(Null))`, so a
> missing property now correctly *is* null. This is now asserted as
> openCypher-correct in the supported corpus (`is_null_absent_prop` /
> `is_not_null_absent_prop` in `src/cypher/compat.rs`), no longer a deviation.

No case in the suite asserts a wrong answer as correct.

---

## 4. Pending — tracked but not yet on trunk

These are *not* part of the supported subset and are *not* yet guarded, so the
suite deliberately contains **no** executing/rejecting case for them. They are
documented here so the gap is explicit rather than silent.

| Construct | Example | Current status | Tracking |
|-----------|---------|----------------|----------|
| Multi-variable, multi-pattern base `MATCH` | `MATCH (a),(b) RETURN a, b` | **Silently wrong on trunk**: converts to an incorrect positional pipeline with no guard. Do not use. | #549 (PR #3507) |
| Plan inspection (`EXPLAIN` / `PROFILE`) | `EXPLAIN MATCH (n) RETURN n` | Not implemented | #562 (PR #3509) |

When #549 lands, `MATCH (a),(b)` moves to either §1 (supported) or §2 (rejected)
and gains suite cases; likewise `EXPLAIN`/`PROFILE` when #562 lands.
