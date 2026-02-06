# Query Pipeline Guide

This guide covers the complete AQL (Gallifrey Query Language) pipeline from parsing to execution.

## Overview

AletheiaDB provides a full query pipeline for executing Cypher-like queries with extensions for vector search and bi-temporal queries:

```
┌─────────────┐     ┌───────────┐     ┌───────────┐     ┌─────────┐     ┌──────────┐
│ AQL String  │ ──► │  Parser   │ ──► │ Converter │ ──► │ Planner │ ──► │ Executor │
│             │     │           │     │           │     │         │     │          │
│ "MATCH..."  │     │ QueryAst  │     │  Query    │     │ Plan    │     │ Results  │
└─────────────┘     └───────────┘     └───────────┘     └─────────┘     └──────────┘
```

## Quick Start

### Basic Query Execution

```rust
use aletheiadb::query::{parse_query, QueryPlanner, QueryExecutor, Statistics};
use aletheiadb::storage::{CurrentStorage, HistoricalStorage};
use std::sync::{Arc, RwLock};

// 1. Parse and convert the query
let query = parse_query("MATCH (n:Person) WHERE n.age > 21 RETURN n LIMIT 10")?;

// 2. Create storage and planner
let current = Arc::new(CurrentStorage::new());
let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
let stats = Arc::new(Statistics::default());
let planner = QueryPlanner::new(stats, Arc::clone(&current));

// 3. Plan the query
let plan = planner.plan(query)?;

// 4. Execute the plan
let executor = QueryExecutor::new(Arc::clone(&current), Arc::clone(&historical));
let results = executor.execute(&plan)?;

// 5. Process results
for row in results {
    let row = row?;
    println!("Found: {:?}", row.entity);
}
```

## Query Types

### 1. Graph Pattern Matching

Match nodes by label:
```cypher
MATCH (n:Person) RETURN n
```

Match with relationships:
```cypher
-- Outgoing relationship
MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN b

-- Incoming relationship
MATCH (a:Person)<-[:FOLLOWS]-(b:Person) RETURN b

-- Any direction
MATCH (a:Person)-[:FRIENDS]-(b:Person) RETURN b
```

Variable-length paths:
```cypher
-- Exactly 2 hops
MATCH (a:Person)-[:KNOWS*2]->(b:Person) RETURN b

-- 1 to 3 hops
MATCH (a:Person)-[:KNOWS*1..3]->(b:Person) RETURN b

-- Any number of hops (use with caution)
MATCH (a:Person)-[:KNOWS*]->(b:Person) RETURN b
```

### 2. Vector Search

k-NN search with literal embedding:
```cypher
SIMILAR TO [0.1, 0.2, 0.3, ...] LIMIT 10
```

k-NN search with parameter:
```rust
let mut params = HashMap::new();
params.insert("emb".to_string(),
    ParameterValue::Embedding(Arc::from(embedding.as_slice())));

let query = parse_query_with_params(
    "SIMILAR TO $emb LIMIT 10",
    params
)?;
```

Find similar to existing node:
```cypher
FIND SIMILAR TO (123) LIMIT 5
```

Hybrid graph + vector:
```cypher
MATCH (n:Document) RANK BY SIMILARITY TO [0.1, 0.2, ...] TOP 10 RETURN n
```

### 3. Temporal Queries

Point-in-time query (valid time only):
```cypher
AS OF 1704067200000000 MATCH (n:Person) RETURN n
```

Point-in-time query (valid time + transaction time):
```cypher
AS OF 1704067200000000, 1704153600000000 MATCH (n:Person) RETURN n
```

Time range query:
```cypher
BETWEEN 1704067200000000 AND 1704153600000000 MATCH (n:Person) RETURN n
```

**Note:** Timestamps are in microseconds since Unix epoch.

### 4. Filtering (WHERE clause)

Comparison operators:
```cypher
WHERE n.age > 21
WHERE n.age >= 21
WHERE n.age < 65
WHERE n.age <= 65
WHERE n.name = 'Alice'
WHERE n.name <> 'Bob'
```

String predicates:
```cypher
WHERE n.bio CONTAINS 'engineer'
WHERE n.name STARTS WITH 'A'
WHERE n.email ENDS WITH '.com'
```

IN list:
```cypher
WHERE n.status IN ['active', 'pending', 'review']
```

Existence checks:
```cypher
WHERE EXISTS(n.email)
WHERE n.deleted IS NULL
WHERE n.verified IS NOT NULL
```

Logical operators:
```cypher
WHERE n.age > 21 AND n.active = true
WHERE n.role = 'admin' OR n.role = 'moderator'
WHERE NOT n.banned = true
WHERE (n.age > 21 AND n.verified = true) OR n.role = 'admin'
```

### 5. Result Modifiers

Projection:
```cypher
RETURN n                    -- Entire node
RETURN n.name, n.age       -- Specific properties
RETURN DISTINCT n          -- Deduplicate
```

Pagination:
```cypher
RETURN n LIMIT 10          -- First 10 results
RETURN n SKIP 20 LIMIT 10  -- Results 21-30
```

## Pipeline Components

### Parser (`aletheiadb::query::Parser`)

Converts AQL strings to AST:

```rust
use aletheiadb::query::Parser;

let ast = Parser::parse("MATCH (n:Person) RETURN n")?;
println!("Source: {:?}", ast.source);
println!("Return: {:?}", ast.return_clause);
```

### Converter (`aletheiadb::query::AstConverter`)

Converts AST to Query (sequence of operations):

```rust
use aletheiadb::query::{Parser, AstConverter, ParameterValue};

let ast = Parser::parse("SIMILAR TO $emb LIMIT 10")?;

let mut converter = AstConverter::new();
converter.bind("emb", ParameterValue::Embedding(embedding));

let query = converter.convert(&ast)?;
```

### Planner (`aletheiadb::query::QueryPlanner`)

Optimizes and creates physical execution plan:

```rust
use aletheiadb::query::{QueryPlanner, Statistics};
use aletheiadb::storage::CurrentStorage;

let storage = Arc::new(CurrentStorage::new());
let stats = Arc::new(Statistics::default());
let planner = QueryPlanner::new(stats, storage);

let plan = planner.plan(query)?;

// View the execution plan
println!("{}", plan.explain());
```

### Executor (`aletheiadb::query::QueryExecutor`)

Executes physical plans:

```rust
use aletheiadb::query::QueryExecutor;

let executor = QueryExecutor::new(current_storage, historical_storage);
let results = executor.execute(&plan)?;
```

## Parameters

### Types of Parameters

| Type | Usage | Example Binding |
|------|-------|-----------------|
| `Embedding` | `SIMILAR TO $emb` | `ParameterValue::Embedding(Arc::from(...))` |
| `NodeId` | `FIND SIMILAR TO ($node)` | `ParameterValue::NodeId(NodeId::new(42)?)` |
| `Value` | `WHERE n.x = $val` | `ParameterValue::Value(PredicateValue::Int(21))` |

### Binding Parameters

Method 1: Using `parse_query_with_params`:
```rust
let mut params = HashMap::new();
params.insert("emb".to_string(), ParameterValue::Embedding(embedding));

let query = parse_query_with_params("SIMILAR TO $emb LIMIT 10", params)?;
```

Method 2: Using `AstConverter::bind`:
```rust
let ast = Parser::parse("FIND SIMILAR TO ($node) LIMIT 5")?;

let mut converter = AstConverter::new();
converter
    .bind("node", ParameterValue::NodeId(node_id))
    .bind("other", ParameterValue::Value(PredicateValue::Int(42)));

let query = converter.convert(&ast)?;
```

## Error Handling

### Parse Errors

```rust
use aletheiadb::query::parse_query;

match parse_query("MATCH (n:Person RETURN n") {
    Ok(_) => unreachable!(),
    Err(e) => {
        // Error contains position and expected token
        println!("Parse error: {}", e);
    }
}
```

### Conversion Errors

```rust
// Missing parameter
let result = parse_query("SIMILAR TO $missing LIMIT 10");
assert!(result.is_err());

// Wrong parameter type
let mut params = HashMap::new();
params.insert("node".to_string(), ParameterValue::Value(PredicateValue::Int(42)));
// Expected NodeId but got Value
let result = parse_query_with_params("FIND SIMILAR TO ($node) LIMIT 5", params);
assert!(result.is_err());
```

### Planning Errors

```rust
// Vector search without index
let query = parse_query("SIMILAR TO [0.1, 0.2] LIMIT 10")?;
let result = planner.plan(query);
// May fail if no vector index exists
```

## Performance Tips

1. **Use parameters** for repeated queries with different values
2. **Add LIMIT** to avoid scanning entire dataset
3. **Use labels** in MATCH patterns to filter early
4. **Avoid variable-length paths** (`*`) without bounds
5. **Place selective predicates first** in WHERE clauses

## Complete Example

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::query::{
    parse_query_with_params, ParameterValue,
    QueryPlanner, QueryExecutor, Statistics
};
use std::collections::HashMap;
use std::sync::Arc;

fn search_similar_documents(
    db: &AletheiaDB,
    query_embedding: Vec<f32>,
    min_score: f64,
    limit: usize,
) -> Result<Vec<NodeId>, Error> {
    // Build query with parameters
    let mut params = HashMap::new();
    params.insert(
        "embedding".to_string(),
        ParameterValue::Embedding(Arc::from(query_embedding.as_slice())),
    );

    let query = parse_query_with_params(
        "SIMILAR TO $embedding LIMIT 100",
        params,
    )?;

    // Plan and execute
    let stats = Arc::new(Statistics::default());
    let planner = QueryPlanner::new(stats, db.current_storage());
    let plan = planner.plan(query)?;

    let executor = QueryExecutor::new(
        db.current_storage(),
        db.historical_storage(),
    );

    // Collect results
    let results = executor.execute(&plan)?;
    let nodes: Vec<NodeId> = results
        .filter_map(|r| r.ok())
        .filter(|r| r.score.unwrap_or(0.0) >= min_score as f32)
        .take(limit)
        .filter_map(|r| r.entity.as_node_id())
        .collect();

    Ok(nodes)
}
```

## See Also

- [Query Language Design](../query-language-design.md) - Full grammar specification
- [Vector Search Guide](vector-search-integration.md) - Detailed vector search documentation
- [Hybrid Query Guide](hybrid-query-guide.md) - Graph + Vector hybrid queries
