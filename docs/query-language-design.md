# Query Language Design for GallifreyDB

This document describes the query language extensions for GallifreyDB, providing a Cypher-like syntax with support for vector search operations, bi-temporal queries, and hybrid graph-vector queries.

## Overview

GallifreyDB's query language (GQL - Gallifrey Query Language) extends the Cypher graph query language with:

1. **Vector Search Operations**: Native k-NN search and similarity-based ranking
2. **Bi-Temporal Queries**: Point-in-time and time-range queries
3. **Hybrid Queries**: Unified syntax combining graph, vector, and temporal operations

## Design Principles

1. **Cypher Compatibility**: Familiar syntax for graph database users
2. **Composability**: Operations can be freely combined
3. **Explicit Semantics**: Clear behavior for all operations
4. **Performance Hints**: Optional syntax for query optimization

## Grammar Specification

### EBNF Grammar

```ebnf
(* Top-level query *)
query           = [ temporal_clause ]
                  ( match_clause | vector_clause )
                  { where_clause }
                  [ return_clause ]
                  [ order_clause ]
                  [ skip_clause ]
                  [ limit_clause ] ;

(* Temporal Clauses *)
temporal_clause = as_of_clause | between_clause ;
as_of_clause    = "AS" "OF" timestamp [ "," timestamp ] ;
between_clause  = "BETWEEN" timestamp "AND" timestamp ;
timestamp       = string_literal | integer_literal ;

(* Match Clause - Graph Pattern Matching *)
match_clause    = "MATCH" pattern { "," pattern } ;
pattern         = node_pattern { relationship_pattern node_pattern } ;
node_pattern    = "(" [ identifier ] [ ":" label ] [ properties ] ")" ;
relationship_pattern = "-[" [ identifier ] [ ":" label ] [ depth_spec ] "]->"
                     | "<-[" [ identifier ] [ ":" label ] [ depth_spec ] "]-"
                     | "-[" [ identifier ] [ ":" label ] [ depth_spec ] "]-" ;
depth_spec      = "*" [ integer_literal [ ".." integer_literal ] ] ;
properties      = "{" property_list "}" ;
property_list   = property { "," property } ;
property        = identifier ":" value ;

(* Vector Clause - Similarity Search *)
vector_clause   = "SIMILAR" "TO" embedding [ "USING" metric ] [ "LIMIT" integer_literal ]
                | "FIND" "SIMILAR" "TO" "(" identifier ")" [ "LIMIT" integer_literal ] ;
embedding       = parameter | "[" float_list "]" ;
float_list      = float_literal { "," float_literal } ;
metric          = "COSINE" | "EUCLIDEAN" | "DOT_PRODUCT" ;

(* Where Clause - Filtering *)
where_clause    = "WHERE" predicate ;
predicate       = comparison
                | existence_check
                | string_predicate
                | predicate "AND" predicate
                | predicate "OR" predicate
                | "NOT" predicate
                | "(" predicate ")" ;
comparison      = expression comp_op expression ;
comp_op         = "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" | "IN" ;
existence_check = "EXISTS" "(" identifier "." identifier ")"
                | identifier "." identifier "IS" [ "NOT" ] "NULL" ;
string_predicate = identifier "." identifier "CONTAINS" string_literal
                 | identifier "." identifier "STARTS" "WITH" string_literal
                 | identifier "." identifier "ENDS" "WITH" string_literal ;

(* Expression *)
expression      = property_access | literal | parameter | function_call ;
property_access = identifier "." identifier ;
literal         = string_literal | integer_literal | float_literal | boolean_literal | "NULL" ;
parameter       = "$" identifier ;
function_call   = identifier "(" [ expression { "," expression } ] ")" ;

(* Return Clause - Projection *)
return_clause   = "RETURN" return_item { "," return_item }
                | "RETURN" "DISTINCT" return_item { "," return_item }
                | "RETURN" "COUNT" "(" ( "*" | identifier ) ")" ;
return_item     = expression [ "AS" identifier ] ;

(* Order Clause - Sorting *)
order_clause    = "ORDER" "BY" order_item { "," order_item } ;
order_item      = expression [ "ASC" | "DESC" ] ;

(* Skip and Limit Clauses *)
skip_clause     = "SKIP" integer_literal ;
limit_clause    = "LIMIT" integer_literal ;

(* Hybrid Operations *)
rank_clause     = "RANK" "BY" "SIMILARITY" "TO" embedding [ "TOP" integer_literal ] ;

(* Identifiers and Literals *)
identifier      = letter { letter | digit | "_" } ;
label           = identifier ;
string_literal  = "'" { character } "'" | '"' { character } '"' ;
integer_literal = [ "-" ] digit { digit } ;
float_literal   = [ "-" ] digit { digit } "." digit { digit } [ exponent ] ;
exponent        = ( "e" | "E" ) [ "+" | "-" ] digit { digit } ;
boolean_literal = "true" | "false" | "TRUE" | "FALSE" ;
letter          = "A" | ... | "Z" | "a" | ... | "z" ;
digit           = "0" | ... | "9" ;
```

## Query Examples

### Basic Graph Queries

```cypher
-- Find a person by name
MATCH (n:Person {name: "Alice"})
RETURN n

-- Find friends of Alice
MATCH (a:Person {name: "Alice"})-[:KNOWS]->(friend:Person)
RETURN friend

-- Multi-hop traversal (friends of friends)
MATCH (a:Person {name: "Alice"})-[:KNOWS*2]->(friend)
RETURN friend

-- Variable depth traversal (1 to 3 hops)
MATCH (a:Person {name: "Alice"})-[:KNOWS*1..3]->(connection)
RETURN DISTINCT connection
```

### Vector Search Queries

```cypher
-- Find 10 most similar nodes to an embedding
SIMILAR TO $embedding LIMIT 10

-- Find similar with specific metric
SIMILAR TO [0.1, 0.2, 0.3, ...] USING COSINE LIMIT 10

-- Find nodes similar to another node
FIND SIMILAR TO (node_id) LIMIT 10
```

### Hybrid Graph + Vector Queries

```cypher
-- Find Alice's friends ranked by similarity to Bob
MATCH (a:Person {name: "Alice"})-[:KNOWS]->(friend)
RANK BY SIMILARITY TO $bob_embedding TOP 10
RETURN friend

-- Find documents similar to a query, then get their authors
SIMILAR TO $query_embedding LIMIT 20
MATCH (doc)<-[:WROTE]-(author:Person)
RETURN author, doc

-- Find similar content within a specific category
MATCH (doc:Document)-[:IN_CATEGORY]->(cat:Category {name: "Science"})
RANK BY SIMILARITY TO $embedding TOP 10
RETURN doc
```

### Bi-Temporal Queries

```cypher
-- Point-in-time query (valid time only)
AS OF '2024-01-15T10:00:00Z'
MATCH (n:Person {name: "Alice"})
RETURN n

-- Bi-temporal query (valid time, transaction time)
AS OF '2024-01-15T10:00:00Z', '2024-01-20T00:00:00Z'
MATCH (n:Person {name: "Alice"})
RETURN n

-- Time range query
BETWEEN '2024-01-01T00:00:00Z' AND '2024-12-31T23:59:59Z'
MATCH (n:Person {name: "Alice"})
RETURN n
```

### Temporal + Vector Queries

```cypher
-- What was similar to this embedding in 2023?
AS OF '2023-06-15T00:00:00Z'
SIMILAR TO $embedding LIMIT 10

-- Track how similar documents changed over time
BETWEEN '2024-01-01' AND '2024-06-30'
MATCH (doc:Document {id: 123})
RETURN doc
```

### Complex Hybrid Queries

```cypher
-- Full hybrid: temporal + graph + vector
AS OF '2024-06-01T00:00:00Z'
MATCH (user:User {id: $user_id})-[:VIEWED]->(item:Product)
RANK BY SIMILARITY TO $recommendation_embedding TOP 20
WHERE item.price < 100
RETURN item, item.name, item.price
ORDER BY score DESC
LIMIT 10
```

## Semantic Mapping

### Query to Internal IR Mapping

| Query Clause | IR Operation |
|--------------|--------------|
| `MATCH (n:Label)` | `QueryOp::ScanNodes { label: Some("Label") }` |
| `MATCH (n {id: X})` | `QueryOp::StartNode(NodeId)` |
| `-[:REL]->` | `QueryOp::TraverseOut { label: Some("REL"), depth: Exact(1) }` |
| `<-[:REL]-` | `QueryOp::TraverseIn { ... }` |
| `-[:REL]-` | `QueryOp::TraverseBoth { ... }` |
| `-[:REL*N]-` | `QueryOp::TraverseOut { ..., depth: Exact(N) }` |
| `-[:REL*M..N]-` | `QueryOp::TraverseOut { ..., depth: Range { min: M, max: N } }` |
| `SIMILAR TO $emb LIMIT K` | `QueryOp::VectorSearch { ..., k: K }` |
| `RANK BY SIMILARITY` | `QueryOp::RankBySimilarity { ... }` |
| `AS OF T1, T2` | `QueryOp::AsOf { valid_time: T1, transaction_time: T2 }` |
| `BETWEEN T1 AND T2` | `QueryOp::Between { time_range: ... }` |
| `WHERE pred` | `QueryOp::Filter(Predicate)` |
| `LIMIT N` | `QueryOp::Limit(N)` |
| `SKIP N` | `QueryOp::Skip(N)` |
| `RETURN DISTINCT` | `QueryOp::Distinct` |
| `RETURN COUNT(*)` | `QueryOp::Count` |
| `RETURN a, b` | `QueryOp::Project(vec!["a", "b"])` |

### Predicate Mapping

| Query Predicate | IR Predicate |
|-----------------|--------------|
| `n.prop = value` | `Predicate::Eq { key, value }` |
| `n.prop <> value` | `Predicate::Ne { key, value }` |
| `n.prop > value` | `Predicate::Gt { key, value }` |
| `n.prop >= value` | `Predicate::Gte { key, value }` |
| `n.prop < value` | `Predicate::Lt { key, value }` |
| `n.prop <= value` | `Predicate::Lte { key, value }` |
| `n.prop IN [...]` | `Predicate::In { key, values }` |
| `n.prop CONTAINS str` | `Predicate::Contains { key, substring }` |
| `n.prop STARTS WITH str` | `Predicate::StartsWith { key, prefix }` |
| `n.prop ENDS WITH str` | `Predicate::EndsWith { key, suffix }` |
| `EXISTS(n.prop)` | `Predicate::Exists(key)` |
| `n.prop IS NULL` | `Predicate::NotExists(key)` |
| `p1 AND p2` | `Predicate::And(vec![p1, p2])` |
| `p1 OR p2` | `Predicate::Or(vec![p1, p2])` |
| `NOT p` | `Predicate::Not(Box::new(p))` |

## Parser Architecture

The parser consists of three main components:

### 1. Lexer (`src/query/lexer.rs`)

Tokenizes input into a stream of tokens:

```rust
pub enum Token {
    // Keywords
    Match, Where, Return, Order, By, Limit, Skip,
    As, Of, Between, And, Or, Not, In, Is, Null,
    Similar, To, Using, Find, Rank, Similarity, Top,
    Distinct, Count, Asc, Desc, True, False,
    Exists, Contains, Starts, Ends, With,
    Cosine, Euclidean, DotProduct,

    // Punctuation
    LeftParen, RightParen,
    LeftBracket, RightBracket,
    LeftBrace, RightBrace,
    Colon, Comma, Dot, Star, Arrow, LeftArrow, Dash,

    // Operators
    Eq, Ne, Lt, Le, Gt, Ge,

    // Literals
    Identifier(String),
    StringLiteral(String),
    IntegerLiteral(i64),
    FloatLiteral(f64),
    Parameter(String),

    // End
    Eof,
}
```

### 2. AST (`src/query/ast.rs`)

Abstract Syntax Tree representing parsed queries:

```rust
pub struct QueryAst {
    pub temporal: Option<TemporalClause>,
    pub source: SourceClause,
    pub where_clauses: Vec<WhereClause>,
    pub return_clause: Option<ReturnClause>,
    pub order: Option<OrderClause>,
    pub skip: Option<usize>,
    pub limit: Option<usize>,
}

pub enum SourceClause {
    Match(Vec<Pattern>),
    VectorSearch { embedding: Embedding, metric: Option<DistanceMetric>, limit: usize },
    FindSimilar { node_ref: NodeRef, limit: usize },
}
```

### 3. Parser (`src/query/parser.rs`)

Recursive descent parser that produces AST from tokens:

```rust
pub struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    pub fn parse(input: &str) -> Result<QueryAst, ParseError>;
}
```

## Error Handling

The parser provides descriptive error messages:

```rust
pub enum ParseError {
    UnexpectedToken { expected: String, found: Token, position: usize },
    UnexpectedEof { expected: String },
    InvalidNumber { value: String, position: usize },
    InvalidEmbedding { reason: String, position: usize },
    InvalidPattern { reason: String, position: usize },
    UnknownKeyword { keyword: String, position: usize },
}
```

Example error messages:

```
Error: Unexpected token at position 15
  Expected: RETURN or LIMIT clause
  Found: 'UNKNOWN'

  MATCH (n:Person) UNKNOWN ...
                   ^^^^^^^
```

## Future Extensions

### SQL:2011 Temporal Syntax

Support for standard temporal SQL syntax:

```sql
-- System time (transaction time)
SELECT * FROM Person FOR SYSTEM_TIME AS OF '2024-01-01'

-- Application time (valid time)
SELECT * FROM Person FOR APPLICATION_TIME AS OF '2024-01-01'
```

### Path Patterns

Support for complex path expressions:

```cypher
-- Named paths
MATCH path = (a)-[:KNOWS*]-(b)
RETURN path

-- Path predicates
MATCH (a)-[path:KNOWS*]-(b)
WHERE length(path) > 3
RETURN a, b
```

### Graph Algorithms

Built-in support for common graph algorithms:

```cypher
-- Shortest path
MATCH shortestPath((a:Person)-[:KNOWS*]-(b:Person))
WHERE a.name = 'Alice' AND b.name = 'Bob'
RETURN path

-- PageRank
CALL pageRank('Person', 'KNOWS')
YIELD node, score
RETURN node.name, score
ORDER BY score DESC
LIMIT 10
```

## References

- [openCypher Specification](https://opencypher.org/)
- [GQL Standard (ISO/IEC 39075)](https://www.gqlstandards.org/)
- [SQL:2011 Temporal Features](https://en.wikipedia.org/wiki/SQL:2011)
- [XTDB Bi-temporality](https://v1-docs.xtdb.com/concepts/bitemporality/)
