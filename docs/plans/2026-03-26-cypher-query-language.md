# Cypher Query Language Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add openCypher query language support with temporal and vector extensions behind the `cypher` feature flag, enabling intuitive graph query syntax for AletheiaDB (Issue #312).

**Architecture:** Hand-written recursive descent parser (zero external deps) produces a Cypher-specific AST that transforms to the existing `Query`/`QueryOp` IR. Follows the same pattern as the `sql` module: feature-gated, own error types, own lexer/parser, converter to shared IR.

**Tech Stack:** Pure Rust, no external parser dependencies. Reuses existing `Query`, `QueryOp`, `Predicate`, `TraversalDepth` from `src/query/ir.rs` and `src/query/builder.rs`.

---

## Architecture Overview

```
Cypher String
    ↓
CypherLexer::tokenize()  [src/cypher/lexer.rs]
    ↓
Vec<Token>
    ↓
CypherParser::parse()     [src/cypher/parser.rs]
    ↓
CypherAst                 [src/cypher/ast.rs]
    ↓
CypherConverter::convert() [src/cypher/converter.rs]
    ↓
Query (Vec<QueryOp>)      [src/query/builder.rs — shared IR]
    ↓
QueryPlanner → QueryExecutor → QueryResults  [existing pipeline]
```

## File Layout

```
src/cypher/          # Behind feature = "cypher" — NO external deps
├── mod.rs           # Module docs, re-exports
├── error.rs         # CypherError type (thiserror)
├── lexer.rs         # Tokenizer: &str → Vec<Token>
├── ast.rs           # Cypher-specific AST types
├── parser.rs        # Recursive descent: Vec<Token> → CypherAst
├── converter.rs     # CypherAst → Query (shared IR)
└── tests.rs         # Unit + integration tests
```

## Conventions

- Follow existing SQL module (`src/sql/`) patterns exactly
- Use `thiserror` for error types (already a dependency)
- All public items get doc comments
- Test-first: write the failing test, then implement
- Each task ends with `cargo clippy --all-features -- -D warnings && cargo fmt --all && cargo test --features cypher`

---

## Task 1: Feature Flag & Module Scaffolding

**Files:**
- Modify: `Cargo.toml` (add `cypher` feature)
- Modify: `src/lib.rs` (add `#[cfg(feature = "cypher")] pub mod cypher;`)
- Create: `src/cypher/mod.rs`
- Create: `src/cypher/error.rs`

**Step 1: Add feature flag to Cargo.toml**

In `Cargo.toml`, after the `sql` feature line (~line 156), add:

```toml
# Cypher query language support (Issue #312)
cypher = []
```

No external dependencies — hand-written parser.

**Step 2: Add conditional module to lib.rs**

In `src/lib.rs`, after the SQL module (~line 83), add:

```rust
// Optional Cypher query language support
#[cfg(feature = "cypher")]
pub mod cypher;
```

**Step 3: Create error types**

Create `src/cypher/error.rs`:

```rust
//! Cypher-specific error types.

use thiserror::Error;

/// Errors that can occur during Cypher parsing and conversion.
#[derive(Debug, Clone, Error)]
pub enum CypherError {
    /// Lexer error (invalid token, unterminated string, etc.).
    #[error("Cypher lexer error at position {position}: {message}")]
    LexError {
        /// Byte position in the input string.
        position: usize,
        /// Human-readable error description.
        message: String,
    },

    /// Parser error (unexpected token, missing clause, etc.).
    #[error("Cypher parse error at position {position}: {message}")]
    ParseError {
        /// Byte position in the input string.
        position: usize,
        /// Human-readable error description.
        message: String,
    },

    /// Unsupported Cypher feature or syntax.
    #[error("Unsupported Cypher feature: {0}")]
    UnsupportedFeature(String),

    /// Invalid temporal clause.
    #[error("Invalid temporal clause: {0}")]
    InvalidTemporalClause(String),

    /// Invalid timestamp format.
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    /// Parameter binding error.
    #[error("Parameter error: {0}")]
    ParameterError(String),

    /// Semantic error detected during AST → Query conversion.
    #[error("Cypher semantic error: {0}")]
    SemanticError(String),
}
```

**Step 4: Create module root**

Create `src/cypher/mod.rs`:

```rust
//! Cypher Query Language Support for AletheiaDB
//!
//! This module provides openCypher-compatible query language support with
//! temporal and vector extensions, enabling intuitive graph query syntax.
//!
//! # Feature Flag
//!
//! This module is only available when the `cypher` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! aletheiadb = { version = "0.1", features = ["cypher"] }
//! ```
//!
//! # Architecture
//!
//! ```text
//! Cypher String → [Lexer] → Tokens → [Parser] → CypherAst → [Converter] → Query
//! ```
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use aletheiadb::cypher::parse_cypher;
//!
//! let query = parse_cypher("MATCH (n:Person {name: 'Alice'}) RETURN n")?;
//! let results = db.execute_query(query)?;
//! ```

mod error;

pub use error::CypherError;

#[cfg(test)]
mod tests;
```

**Step 5: Create empty test file**

Create `src/cypher/tests.rs`:

```rust
//! Tests for the Cypher query language module.

use super::*;

#[test]
fn test_module_compiles() {
    // Smoke test: the cypher module compiles with the feature flag
    let _err = CypherError::UnsupportedFeature("test".to_string());
    assert!(matches!(_err, CypherError::UnsupportedFeature(_)));
}
```

**Step 6: Verify**

```bash
cargo test --features cypher -- cypher::tests::test_module_compiles
cargo clippy --all-features -- -D warnings
cargo fmt --all
```

**Step 7: Commit**

```bash
git add Cargo.toml src/lib.rs src/cypher/
git commit -m "feat(cypher): add feature flag and module scaffolding (#312)

Phase 1 foundation: cypher feature flag, error types, module structure.
Zero external dependencies."
```

---

## Task 2: Cypher Lexer

**Files:**
- Create: `src/cypher/lexer.rs`
- Modify: `src/cypher/mod.rs` (add `mod lexer`)
- Modify: `src/cypher/tests.rs` (add lexer tests)

**Step 1: Write failing tests for the lexer**

Add to `src/cypher/tests.rs`:

```rust
use super::lexer::{CypherLexer, Token, TokenKind};

// === Lexer Tests ===

#[test]
fn test_lex_empty() {
    let tokens = CypherLexer::tokenize("").unwrap();
    assert_eq!(tokens.len(), 1); // just EOF
    assert_eq!(tokens[0].kind, TokenKind::Eof);
}

#[test]
fn test_lex_keywords() {
    let tokens = CypherLexer::tokenize("MATCH RETURN WHERE").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Match);
    assert_eq!(tokens[1].kind, TokenKind::Return);
    assert_eq!(tokens[2].kind, TokenKind::Where);
}

#[test]
fn test_lex_case_insensitive() {
    let tokens = CypherLexer::tokenize("match RETURN Where").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Match);
    assert_eq!(tokens[1].kind, TokenKind::Return);
    assert_eq!(tokens[2].kind, TokenKind::Where);
}

#[test]
fn test_lex_identifier() {
    let tokens = CypherLexer::tokenize("myVar").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Identifier);
    assert_eq!(tokens[0].text, "myVar");
}

#[test]
fn test_lex_string_single_quotes() {
    let tokens = CypherLexer::tokenize("'hello world'").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
    assert_eq!(tokens[0].text, "hello world");
}

#[test]
fn test_lex_string_double_quotes() {
    let tokens = CypherLexer::tokenize("\"hello\"").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral);
    assert_eq!(tokens[0].text, "hello");
}

#[test]
fn test_lex_integer() {
    let tokens = CypherLexer::tokenize("42").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntegerLiteral);
    assert_eq!(tokens[0].text, "42");
}

#[test]
fn test_lex_float() {
    let tokens = CypherLexer::tokenize("3.14").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::FloatLiteral);
    assert_eq!(tokens[0].text, "3.14");
}

#[test]
fn test_lex_symbols() {
    let tokens = CypherLexer::tokenize("()[]{}:.,->-<-").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::LParen);
    assert_eq!(tokens[1].kind, TokenKind::RParen);
    assert_eq!(tokens[2].kind, TokenKind::LBracket);
    assert_eq!(tokens[3].kind, TokenKind::RBracket);
    assert_eq!(tokens[4].kind, TokenKind::LBrace);
    assert_eq!(tokens[5].kind, TokenKind::RBrace);
    assert_eq!(tokens[6].kind, TokenKind::Colon);
    assert_eq!(tokens[7].kind, TokenKind::Dot);
    assert_eq!(tokens[8].kind, TokenKind::Comma);
    assert_eq!(tokens[9].kind, TokenKind::Arrow);    // ->
    assert_eq!(tokens[10].kind, TokenKind::Dash);     // -
    assert_eq!(tokens[11].kind, TokenKind::LeftArrow); // <-
}

#[test]
fn test_lex_comparison_operators() {
    let tokens = CypherLexer::tokenize("= <> < <= > >=").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Eq);
    assert_eq!(tokens[1].kind, TokenKind::Ne);
    assert_eq!(tokens[2].kind, TokenKind::Lt);
    assert_eq!(tokens[3].kind, TokenKind::Le);
    assert_eq!(tokens[4].kind, TokenKind::Gt);
    assert_eq!(tokens[5].kind, TokenKind::Ge);
}

#[test]
fn test_lex_parameter() {
    let tokens = CypherLexer::tokenize("$myParam").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Parameter);
    assert_eq!(tokens[0].text, "myParam");
}

#[test]
fn test_lex_star() {
    let tokens = CypherLexer::tokenize("*").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Star);
}

#[test]
fn test_lex_dotdot() {
    let tokens = CypherLexer::tokenize("1..3").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntegerLiteral);
    assert_eq!(tokens[1].kind, TokenKind::DotDot);
    assert_eq!(tokens[2].kind, TokenKind::IntegerLiteral);
}

#[test]
fn test_lex_full_query() {
    let q = "MATCH (n:Person {name: 'Alice'})-[:KNOWS]->(m) RETURN m LIMIT 10";
    let tokens = CypherLexer::tokenize(q).unwrap();
    // Should tokenize without error; verify first/last
    assert_eq!(tokens[0].kind, TokenKind::Match);
    assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
}

#[test]
fn test_lex_comment_line() {
    let tokens = CypherLexer::tokenize("MATCH // comment\n(n)").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Match);
    assert_eq!(tokens[1].kind, TokenKind::LParen);
}

#[test]
fn test_lex_unterminated_string() {
    let result = CypherLexer::tokenize("'unterminated");
    assert!(result.is_err());
}

#[test]
fn test_lex_boolean_and_null() {
    let tokens = CypherLexer::tokenize("true false null").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::True);
    assert_eq!(tokens[1].kind, TokenKind::False);
    assert_eq!(tokens[2].kind, TokenKind::Null);
}

#[test]
fn test_lex_temporal_keywords() {
    let tokens = CypherLexer::tokenize("AS OF TIMESTAMP BETWEEN AND").unwrap();
    assert_eq!(tokens[0].kind, TokenKind::As);
    assert_eq!(tokens[1].kind, TokenKind::Of);
    assert_eq!(tokens[2].kind, TokenKind::Timestamp);
    assert_eq!(tokens[3].kind, TokenKind::Between);
    assert_eq!(tokens[4].kind, TokenKind::And);
}

#[test]
fn test_lex_vector_dot_function() {
    let tokens = CypherLexer::tokenize("vector.similarity").unwrap();
    // "vector" = identifier, "." = Dot, "similarity" = identifier
    assert_eq!(tokens[0].kind, TokenKind::Identifier);
    assert_eq!(tokens[0].text, "vector");
    assert_eq!(tokens[1].kind, TokenKind::Dot);
    assert_eq!(tokens[2].kind, TokenKind::Identifier);
    assert_eq!(tokens[2].text, "similarity");
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test --features cypher -- cypher::tests 2>&1 | head -20
# Expected: compilation error — `lexer` module not found
```

**Step 3: Implement the lexer**

Create `src/cypher/lexer.rs` with:

- `TokenKind` enum: all keywords (MATCH, WHERE, RETURN, OPTIONAL, WITH, ORDER, BY, LIMIT, SKIP, ASC, DESC, AS, OF, TIMESTAMP, BETWEEN, AND, OR, NOT, IN, IS, NULL, TRUE, FALSE, DISTINCT, COUNT, AVG, SUM, COLLECT, UNWIND, FOR, SYSTEM_TIME, VALID_TIME), operators (Eq, Ne, Lt, Le, Gt, Ge), symbols (LParen, RParen, LBracket, RBracket, LBrace, RBrace, Colon, Dot, DotDot, Comma, Dash, Arrow, LeftArrow, Star, Pipe, Plus, Slash, Percent), literals (IntegerLiteral, FloatLiteral, StringLiteral, Parameter, Identifier), and Eof.
- `Token` struct: `kind: TokenKind`, `text: String`, `position: usize`
- `CypherLexer` struct with `tokenize(input: &str) -> Result<Vec<Token>, CypherError>`:
  - Skip whitespace and comments (`//` to end-of-line)
  - Case-insensitive keyword matching (scan identifier, uppercase-compare to keyword table)
  - String literals with both `'` and `"` delimiters, with `\'` / `\"` escape support
  - Integer and float literals
  - Parameters: `$` followed by identifier chars
  - Multi-char symbols: `->`, `<-`, `<>`, `<=`, `>=`, `..`, `!=`
  - Single-char symbols: `()[]{}:.,*=<>-+/%|`

Add to `src/cypher/mod.rs`: `mod lexer;` and `pub use lexer::{CypherLexer, Token, TokenKind};`

**Step 4: Run tests to verify they pass**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings
cargo fmt --all
```

**Step 5: Commit**

```bash
git add src/cypher/lexer.rs src/cypher/mod.rs src/cypher/tests.rs
git commit -m "feat(cypher): implement Cypher lexer (#312)

Hand-written tokenizer with case-insensitive keywords, string literals,
parameters, comparison operators, and all Cypher symbols."
```

---

## Task 3: Cypher AST Types

**Files:**
- Create: `src/cypher/ast.rs`
- Modify: `src/cypher/mod.rs` (add `mod ast`)

**Step 1: Write AST construction tests**

Add to `src/cypher/tests.rs`:

```rust
use super::ast::*;

#[test]
fn test_cypher_ast_basic_match() {
    let ast = CypherStatement::Match {
        optional: false,
        pattern: vec![CypherPattern {
            elements: vec![
                CypherPatternElement::Node(CypherNodePattern {
                    variable: Some("n".into()),
                    labels: vec!["Person".into()],
                    properties: vec![],
                }),
            ],
        }],
        where_clause: None,
        return_clause: CypherReturn {
            distinct: false,
            items: vec![CypherReturnItem::Variable("n".into())],
            order_by: vec![],
            skip: None,
            limit: None,
        },
        temporal: None,
        with_clauses: vec![],
    };
    assert!(matches!(ast, CypherStatement::Match { .. }));
}

#[test]
fn test_cypher_pattern_chain() {
    let pattern = CypherPattern {
        elements: vec![
            CypherPatternElement::Node(CypherNodePattern {
                variable: Some("a".into()),
                labels: vec!["Person".into()],
                properties: vec![],
            }),
            CypherPatternElement::Relationship(CypherRelPattern {
                variable: None,
                rel_types: vec!["KNOWS".into()],
                direction: CypherDirection::Outgoing,
                depth: None,
                properties: vec![],
            }),
            CypherPatternElement::Node(CypherNodePattern {
                variable: Some("b".into()),
                labels: vec![],
                properties: vec![],
            }),
        ],
    };
    assert_eq!(pattern.elements.len(), 3);
}
```

**Step 2: Implement AST types**

Create `src/cypher/ast.rs` with:

```rust
//! Cypher Abstract Syntax Tree types.
//!
//! These types represent parsed Cypher queries before conversion
//! to the AletheiaDB internal query representation.

use std::sync::Arc;

/// A complete Cypher statement.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherStatement {
    /// MATCH ... [WHERE ...] RETURN ...
    Match {
        /// Is this an OPTIONAL MATCH?
        optional: bool,
        /// Graph patterns to match
        pattern: Vec<CypherPattern>,
        /// Optional WHERE clause
        where_clause: Option<CypherExpr>,
        /// RETURN clause (required)
        return_clause: CypherReturn,
        /// Optional temporal clause (AS OF, BETWEEN, etc.)
        temporal: Option<CypherTemporal>,
        /// Optional WITH clauses (query chaining)
        with_clauses: Vec<CypherWith>,
    },
}

/// A graph pattern (sequence of nodes and relationships).
#[derive(Debug, Clone, PartialEq)]
pub struct CypherPattern {
    /// Alternating node and relationship elements.
    pub elements: Vec<CypherPatternElement>,
}

/// An element in a graph pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherPatternElement {
    /// A node: `(var:Label {props})`
    Node(CypherNodePattern),
    /// A relationship: `-[var:TYPE*depth]->`
    Relationship(CypherRelPattern),
}

/// A node pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherNodePattern {
    /// Optional variable binding: `(n:...)`
    pub variable: Option<String>,
    /// Zero or more labels: `(:Person:Employee)`
    pub labels: Vec<String>,
    /// Inline properties: `{name: 'Alice', age: 30}`
    pub properties: Vec<(String, CypherValue)>,
}

/// A relationship pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherRelPattern {
    /// Optional variable binding: `-[r:...]->`
    pub variable: Option<String>,
    /// Zero or more relationship types: `[:KNOWS|FOLLOWS]`
    pub rel_types: Vec<String>,
    /// Direction of the relationship.
    pub direction: CypherDirection,
    /// Optional depth for variable-length paths: `*1..3`
    pub depth: Option<CypherDepth>,
    /// Inline properties (rare, but Cypher supports it).
    pub properties: Vec<(String, CypherValue)>,
}

/// Direction of a relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CypherDirection {
    /// `->` outgoing
    Outgoing,
    /// `<-` incoming
    Incoming,
    /// `-` bidirectional (undirected)
    Both,
}

/// Depth specification for variable-length paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CypherDepth {
    /// `*` — unbounded
    Unbounded,
    /// `*3` — exactly N
    Exact(usize),
    /// `*..5` — at most N
    Max(usize),
    /// `*2..` — at least N (unbounded upper)
    Min(usize),
    /// `*1..3` — range
    Range { min: usize, max: usize },
}

/// A literal or parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherValue {
    /// Null
    Null,
    /// Boolean
    Bool(bool),
    /// Integer
    Int(i64),
    /// Float
    Float(f64),
    /// String
    String(String),
    /// Parameter reference: `$name`
    Parameter(String),
    /// Embedding vector: `[0.1, 0.2, ...]`
    Vector(Arc<[f32]>),
}

/// An expression (used in WHERE, WITH, RETURN, ORDER BY).
#[derive(Debug, Clone, PartialEq)]
pub enum CypherExpr {
    /// Literal value
    Value(CypherValue),
    /// Variable reference: `n`
    Variable(String),
    /// Property access: `n.name`
    Property { variable: String, property: String },
    /// Comparison: `expr op expr`
    Comparison {
        left: Box<CypherExpr>,
        op: CypherCompOp,
        right: Box<CypherExpr>,
    },
    /// AND
    And(Box<CypherExpr>, Box<CypherExpr>),
    /// OR
    Or(Box<CypherExpr>, Box<CypherExpr>),
    /// NOT
    Not(Box<CypherExpr>),
    /// IS NULL
    IsNull(Box<CypherExpr>),
    /// IS NOT NULL
    IsNotNull(Box<CypherExpr>),
    /// IN [values]
    In {
        expr: Box<CypherExpr>,
        values: Vec<CypherExpr>,
    },
    /// CONTAINS
    Contains {
        expr: Box<CypherExpr>,
        substring: String,
    },
    /// STARTS WITH
    StartsWith {
        expr: Box<CypherExpr>,
        prefix: String,
    },
    /// ENDS WITH
    EndsWith {
        expr: Box<CypherExpr>,
        suffix: String,
    },
    /// Function call: `vector.similarity(a.emb, $emb)`
    FunctionCall {
        name: String,
        args: Vec<CypherExpr>,
    },
    /// Parenthesized expression
    Grouped(Box<CypherExpr>),
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CypherCompOp {
    /// `=`
    Eq,
    /// `<>` or `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// RETURN clause.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherReturn {
    /// DISTINCT modifier
    pub distinct: bool,
    /// Items to return
    pub items: Vec<CypherReturnItem>,
    /// ORDER BY
    pub order_by: Vec<CypherOrderItem>,
    /// SKIP
    pub skip: Option<usize>,
    /// LIMIT
    pub limit: Option<usize>,
}

/// A RETURN item.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherReturnItem {
    /// Return all: `*`
    Star,
    /// Return a variable: `n`
    Variable(String),
    /// Return an expression with optional alias: `n.name AS personName`
    Expression {
        expr: CypherExpr,
        alias: Option<String>,
    },
}

/// ORDER BY item.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherOrderItem {
    /// Expression to order by
    pub expr: CypherExpr,
    /// true = DESC, false = ASC (default)
    pub descending: bool,
}

/// Temporal clause (AletheiaDB extension).
#[derive(Debug, Clone, PartialEq)]
pub enum CypherTemporal {
    /// `AS OF TIMESTAMP 'time'`
    AsOfTimestamp(String),
    /// `AS OF VALID_TIME 'time'`
    AsOfValidTime(String),
    /// `AS OF SYSTEM_TIME 'time'` / `FOR SYSTEM_TIME AS OF 'time'`
    AsOfSystemTime(String),
    /// `AS OF VALID_TIME 'vt' AS OF SYSTEM_TIME 'st'` (bi-temporal)
    BiTemporal {
        valid_time: String,
        system_time: String,
    },
    /// `BETWEEN 'start' AND 'end'`
    Between { start: String, end: String },
}

/// WITH clause (query chaining).
#[derive(Debug, Clone, PartialEq)]
pub struct CypherWith {
    /// Items to pass forward
    pub items: Vec<CypherReturnItem>,
    /// Optional WHERE clause on the WITH
    pub where_clause: Option<CypherExpr>,
}
```

Add to `src/cypher/mod.rs`: `mod ast;` and `pub use ast::*;`

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/cypher/ast.rs src/cypher/mod.rs src/cypher/tests.rs
git commit -m "feat(cypher): define Cypher AST types (#312)

Complete AST: patterns, expressions, temporal/vector extensions,
WITH clause, RETURN/ORDER BY/LIMIT, and parameter support."
```

---

## Task 4: Cypher Parser — Basic MATCH + RETURN + WHERE + LIMIT

**Files:**
- Create: `src/cypher/parser.rs`
- Modify: `src/cypher/mod.rs` (add `mod parser`)
- Modify: `src/cypher/tests.rs` (add parser tests)

**Step 1: Write failing parser tests**

Add to `src/cypher/tests.rs`:

```rust
use super::parser::CypherParser;

// === Parser Tests ===

#[test]
fn test_parse_simple_match() {
    let ast = CypherParser::parse("MATCH (n) RETURN n").unwrap();
    if let CypherStatement::Match { pattern, return_clause, .. } = ast {
        assert_eq!(pattern.len(), 1);
        assert_eq!(pattern[0].elements.len(), 1);
        assert_eq!(return_clause.items.len(), 1);
    } else {
        panic!("Expected Match statement");
    }
}

#[test]
fn test_parse_match_with_label() {
    let ast = CypherParser::parse("MATCH (n:Person) RETURN n").unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Node(ref node) = pattern[0].elements[0] {
            assert_eq!(node.labels, vec!["Person"]);
            assert_eq!(node.variable, Some("n".into()));
        } else {
            panic!("Expected Node pattern");
        }
    }
}

#[test]
fn test_parse_match_with_properties() {
    let ast = CypherParser::parse(
        "MATCH (n:Person {name: 'Alice', age: 30}) RETURN n"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Node(ref node) = pattern[0].elements[0] {
            assert_eq!(node.properties.len(), 2);
            assert_eq!(node.properties[0].0, "name");
            assert_eq!(node.properties[0].1, CypherValue::String("Alice".into()));
            assert_eq!(node.properties[1].0, "age");
            assert_eq!(node.properties[1].1, CypherValue::Int(30));
        }
    }
}

#[test]
fn test_parse_traversal() {
    let ast = CypherParser::parse(
        "MATCH (a:Person)-[:KNOWS]->(b) RETURN b"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        assert_eq!(pattern[0].elements.len(), 3); // node, rel, node
        if let CypherPatternElement::Relationship(ref rel) = pattern[0].elements[1] {
            assert_eq!(rel.rel_types, vec!["KNOWS"]);
            assert_eq!(rel.direction, CypherDirection::Outgoing);
        }
    }
}

#[test]
fn test_parse_incoming_relationship() {
    let ast = CypherParser::parse(
        "MATCH (a)<-[:FOLLOWS]-(b) RETURN b"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Relationship(ref rel) = pattern[0].elements[1] {
            assert_eq!(rel.direction, CypherDirection::Incoming);
        }
    }
}

#[test]
fn test_parse_bidirectional_relationship() {
    let ast = CypherParser::parse(
        "MATCH (a)-[:KNOWS]-(b) RETURN b"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Relationship(ref rel) = pattern[0].elements[1] {
            assert_eq!(rel.direction, CypherDirection::Both);
        }
    }
}

#[test]
fn test_parse_variable_length_path() {
    let ast = CypherParser::parse(
        "MATCH (a)-[:KNOWS*1..3]->(b) RETURN b"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Relationship(ref rel) = pattern[0].elements[1] {
            assert_eq!(rel.depth, Some(CypherDepth::Range { min: 1, max: 3 }));
        }
    }
}

#[test]
fn test_parse_unbounded_path() {
    let ast = CypherParser::parse(
        "MATCH (a)-[:KNOWS*]->(b) RETURN b"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Relationship(ref rel) = pattern[0].elements[1] {
            assert_eq!(rel.depth, Some(CypherDepth::Unbounded));
        }
    }
}

#[test]
fn test_parse_where_clause() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) WHERE n.age > 18 RETURN n"
    ).unwrap();
    if let CypherStatement::Match { where_clause, .. } = ast {
        assert!(where_clause.is_some());
    }
}

#[test]
fn test_parse_where_and() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) WHERE n.age > 18 AND n.name = 'Alice' RETURN n"
    ).unwrap();
    if let CypherStatement::Match { where_clause: Some(expr), .. } = ast {
        assert!(matches!(expr, CypherExpr::And(_, _)));
    }
}

#[test]
fn test_parse_limit() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) RETURN n LIMIT 10"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert_eq!(return_clause.limit, Some(10));
    }
}

#[test]
fn test_parse_skip_limit() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) RETURN n SKIP 5 LIMIT 10"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert_eq!(return_clause.skip, Some(5));
        assert_eq!(return_clause.limit, Some(10));
    }
}

#[test]
fn test_parse_order_by() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) RETURN n ORDER BY n.age DESC LIMIT 10"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert_eq!(return_clause.order_by.len(), 1);
        assert!(return_clause.order_by[0].descending);
    }
}

#[test]
fn test_parse_return_distinct() {
    let ast = CypherParser::parse(
        "MATCH (n:Person)-[:KNOWS]->(m) RETURN DISTINCT m"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert!(return_clause.distinct);
    }
}

#[test]
fn test_parse_return_expression_with_alias() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) RETURN n.name AS personName, n.age"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert_eq!(return_clause.items.len(), 2);
        if let CypherReturnItem::Expression { alias, .. } = &return_clause.items[0] {
            assert_eq!(alias.as_deref(), Some("personName"));
        }
    }
}

#[test]
fn test_parse_return_star() {
    let ast = CypherParser::parse("MATCH (n:Person) RETURN *").unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert!(matches!(return_clause.items[0], CypherReturnItem::Star));
    }
}

#[test]
fn test_parse_parameter() {
    let ast = CypherParser::parse(
        "MATCH (n:Person {name: $name}) RETURN n"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        if let CypherPatternElement::Node(ref node) = pattern[0].elements[0] {
            assert_eq!(node.properties[0].1, CypherValue::Parameter("name".into()));
        }
    }
}

#[test]
fn test_parse_error_missing_return() {
    let result = CypherParser::parse("MATCH (n:Person)");
    assert!(result.is_err());
}

#[test]
fn test_parse_error_invalid_syntax() {
    let result = CypherParser::parse("MATCH RETURN");
    assert!(result.is_err());
}
```

**Step 2: Implement the parser**

Create `src/cypher/parser.rs` implementing a recursive descent parser:

Key structure:
```rust
pub struct CypherParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl CypherParser {
    pub fn parse(input: &str) -> Result<CypherStatement, CypherError> { ... }

    // Utility methods
    fn peek(&self) -> &Token { ... }
    fn advance(&mut self) -> &Token { ... }
    fn expect(&mut self, kind: TokenKind) -> Result<&Token, CypherError> { ... }
    fn at(&self, kind: TokenKind) -> bool { ... }
    fn eat(&mut self, kind: TokenKind) -> bool { ... }

    // Grammar rules — each returns a parsed AST node
    fn parse_statement(&mut self) -> Result<CypherStatement, CypherError> { ... }
    fn parse_match(&mut self) -> Result<CypherStatement, CypherError> { ... }
    fn parse_pattern(&mut self) -> Result<CypherPattern, CypherError> { ... }
    fn parse_node_pattern(&mut self) -> Result<CypherNodePattern, CypherError> { ... }
    fn parse_relationship_pattern(&mut self) -> Result<CypherRelPattern, CypherError> { ... }
    fn parse_depth(&mut self) -> Result<CypherDepth, CypherError> { ... }
    fn parse_properties(&mut self) -> Result<Vec<(String, CypherValue)>, CypherError> { ... }
    fn parse_where(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_expression(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_or_expr(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_and_expr(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_not_expr(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_comparison(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_primary_expr(&mut self) -> Result<CypherExpr, CypherError> { ... }
    fn parse_return(&mut self) -> Result<CypherReturn, CypherError> { ... }
    fn parse_return_item(&mut self) -> Result<CypherReturnItem, CypherError> { ... }
    fn parse_order_by(&mut self) -> Result<Vec<CypherOrderItem>, CypherError> { ... }
    fn parse_value(&mut self) -> Result<CypherValue, CypherError> { ... }
}
```

Grammar (LL(1) recursive descent):

```
statement    := [temporal] match_stmt
match_stmt   := MATCH pattern_list [where_clause] return_clause
pattern_list := pattern (',' pattern)*
pattern      := node_pattern (rel_pattern node_pattern)*
node_pattern := '(' [var] [':' label]* ['{' props '}'] ')'
rel_pattern  := '-' '[' [var] [':' type ('|' type)*] ['*' depth] ']' '->'
              | '<-' '[' ... ']' '-'
              | '-' '[' ... ']' '-'
depth        := ε | N | N '..' M | '..' M | N '..'
where_clause := WHERE expr
return_clause:= RETURN [DISTINCT] return_items [order_by] [SKIP n] [LIMIT n]
return_items := '*' | return_item (',' return_item)*
return_item  := expr [AS identifier]
order_by     := ORDER BY order_item (',' order_item)*
order_item   := expr [ASC | DESC]
expr         := or_expr
or_expr      := and_expr (OR and_expr)*
and_expr     := not_expr (AND not_expr)*
not_expr     := NOT not_expr | comparison
comparison   := primary (comp_op primary)?
comp_op      := '=' | '<>' | '!=' | '<' | '<=' | '>' | '>='
primary      := value | var '.' prop | '(' expr ')' | func_call | var
func_call    := name '(' [expr (',' expr)*] ')'
value        := string | integer | float | true | false | null | $param | '[' ...]
props        := key ':' value (',' key ':' value)*
```

Add to `src/cypher/mod.rs`: `mod parser;` and `pub use parser::CypherParser;`

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/cypher/parser.rs src/cypher/mod.rs src/cypher/tests.rs
git commit -m "feat(cypher): implement recursive descent parser (#312)

Parses MATCH with node/relationship patterns, WHERE with boolean
expressions, RETURN with aliases/DISTINCT, ORDER BY, SKIP, LIMIT.
Variable-length paths (*1..3) and bidirectional patterns supported."
```

---

## Task 5: AST → Query Converter

**Files:**
- Create: `src/cypher/converter.rs`
- Modify: `src/cypher/mod.rs` (add `mod converter`, public API)
- Modify: `src/cypher/tests.rs` (add converter tests)

**Step 1: Write failing converter tests**

Add to `src/cypher/tests.rs`:

```rust
use super::{parse_cypher, parse_cypher_with_params, CypherParameterValue};
use crate::query::ir::{QueryOp, Predicate, TraversalDepth};

// === Converter Tests ===

#[test]
fn test_convert_simple_scan() {
    let query = parse_cypher("MATCH (n:Person) RETURN n").unwrap();
    // Should produce: ScanNodes { label: Some("Person") }
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::ScanNodes { label: Some(l) } if l == "Person")));
}

#[test]
fn test_convert_scan_all() {
    let query = parse_cypher("MATCH (n) RETURN n").unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::ScanNodes { label: None })));
}

#[test]
fn test_convert_property_filter() {
    let query = parse_cypher(
        "MATCH (n:Person {name: 'Alice'}) RETURN n"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(Predicate::Eq { .. }))));
}

#[test]
fn test_convert_where_filter() {
    let query = parse_cypher(
        "MATCH (n:Person) WHERE n.age > 18 RETURN n"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(Predicate::Gt { .. }))));
}

#[test]
fn test_convert_traversal() {
    let query = parse_cypher(
        "MATCH (a:Person)-[:KNOWS]->(b) RETURN b"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(
        op,
        QueryOp::TraverseOut { label: Some(l), .. } if l == "KNOWS"
    )));
}

#[test]
fn test_convert_incoming_traversal() {
    let query = parse_cypher(
        "MATCH (a)<-[:FOLLOWS]-(b) RETURN b"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(
        op,
        QueryOp::TraverseIn { label: Some(l), .. } if l == "FOLLOWS"
    )));
}

#[test]
fn test_convert_bidirectional_traversal() {
    let query = parse_cypher(
        "MATCH (a)-[:KNOWS]-(b) RETURN b"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::TraverseBoth { .. })));
}

#[test]
fn test_convert_variable_length() {
    let query = parse_cypher(
        "MATCH (a)-[:KNOWS*1..3]->(b) RETURN b"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(
        op,
        QueryOp::TraverseOut { depth: TraversalDepth::Range { min: 1, max: 3 }, .. }
    )));
}

#[test]
fn test_convert_limit() {
    let query = parse_cypher("MATCH (n:Person) RETURN n LIMIT 10").unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(10))));
}

#[test]
fn test_convert_skip() {
    let query = parse_cypher("MATCH (n:Person) RETURN n SKIP 5").unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Skip(5))));
}

#[test]
fn test_convert_distinct() {
    let query = parse_cypher(
        "MATCH (a)-[:KNOWS]->(b) RETURN DISTINCT b"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Distinct)));
}

#[test]
fn test_convert_order_by() {
    let query = parse_cypher(
        "MATCH (n:Person) RETURN n ORDER BY n.age DESC LIMIT 10"
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Sort { descending: true, .. })));
}

#[test]
fn test_convert_with_params() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    params.insert("name".to_string(), CypherParameterValue::String("Alice".into()));
    let query = parse_cypher_with_params(
        "MATCH (n:Person {name: $name}) RETURN n",
        params,
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(Predicate::Eq { .. }))));
}
```

**Step 2: Implement the converter**

Create `src/cypher/converter.rs`:

```rust
//! Converts Cypher AST to AletheiaDB's internal Query representation.

use std::collections::HashMap;
use std::sync::Arc;

use crate::query::builder::Query;
use crate::query::ir::*;

use super::ast::*;
use super::error::CypherError;

/// Parameter values for Cypher queries.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherParameterValue {
    /// Null
    Null,
    /// Boolean
    Bool(bool),
    /// Integer
    Int(i64),
    /// Float
    Float(f64),
    /// String
    String(String),
    /// Embedding vector for vector search
    Embedding(Arc<[f32]>),
}

/// Converts a Cypher AST into an AletheiaDB Query.
pub struct CypherConverter {
    params: HashMap<String, CypherParameterValue>,
}

impl CypherConverter {
    pub fn new() -> Self { ... }
    pub fn with_params(params: HashMap<String, CypherParameterValue>) -> Self { ... }

    pub fn convert(&self, stmt: CypherStatement) -> Result<Query, CypherError> { ... }

    // Internal conversion methods:
    fn convert_match(&self, ...) -> Result<Query, CypherError> { ... }
    fn convert_pattern(&self, pattern: &CypherPattern) -> Result<Vec<QueryOp>, CypherError> { ... }
    fn convert_node_pattern(&self, node: &CypherNodePattern) -> Result<Vec<QueryOp>, CypherError> { ... }
    fn convert_rel_pattern(&self, rel: &CypherRelPattern) -> Result<QueryOp, CypherError> { ... }
    fn convert_where(&self, expr: &CypherExpr) -> Result<Predicate, CypherError> { ... }
    fn convert_expr_to_predicate(&self, expr: &CypherExpr) -> Result<Predicate, CypherError> { ... }
    fn convert_temporal(&self, temporal: &CypherTemporal) -> Result<QueryOp, CypherError> { ... }
    fn resolve_param(&self, name: &str) -> Result<PredicateValue, CypherError> { ... }
    fn resolve_value(&self, value: &CypherValue) -> Result<PredicateValue, CypherError> { ... }
    fn convert_depth(&self, depth: &CypherDepth) -> TraversalDepth { ... }
}

/// Parse a Cypher query string into an AletheiaDB Query.
pub fn parse_cypher(query: &str) -> Result<Query, CypherError> {
    let ast = CypherParser::parse(query)?;
    let converter = CypherConverter::new();
    converter.convert(ast)
}

/// Parse a Cypher query string with parameter bindings.
pub fn parse_cypher_with_params(
    query: &str,
    params: HashMap<String, CypherParameterValue>,
) -> Result<Query, CypherError> {
    let ast = CypherParser::parse(query)?;
    let converter = CypherConverter::with_params(params);
    converter.convert(ast)
}
```

Conversion rules:
- `MATCH (n:Label)` → `ScanNodes { label: Some("Label") }`
- `MATCH (n)` → `ScanNodes { label: None }`
- `{name: 'Alice'}` → `Filter(Predicate::Eq { key: "name", value: "Alice" })`
- `-[:REL]->` → `TraverseOut { label: Some("REL"), depth: Exact(1) }`
- `<-[:REL]-` → `TraverseIn { ... }`
- `-[:REL]-` → `TraverseBoth { ... }`
- `*1..3` → `TraversalDepth::Range { min: 1, max: 3 }`
- `WHERE n.age > 18` → `Filter(Predicate::Gt { key: "age", value: Int(18) })`
- `WHERE a AND b` → `Filter(Predicate::And(...))`
- `LIMIT n` → `Limit(n)`
- `SKIP n` → `Skip(n)`
- `DISTINCT` → `Distinct`
- `ORDER BY n.x DESC` → `Sort { key: Property("x"), descending: true }`
- `$param` → Resolve from params HashMap

Update `src/cypher/mod.rs`:
```rust
mod converter;
pub use converter::{CypherConverter, CypherParameterValue, parse_cypher, parse_cypher_with_params};
```

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/cypher/converter.rs src/cypher/mod.rs src/cypher/tests.rs
git commit -m "feat(cypher): implement AST to Query converter (#312)

Converts Cypher patterns to QueryOps: node scans, traversals (in/out/both),
property filters, WHERE predicates, LIMIT, SKIP, DISTINCT, ORDER BY.
Parameter binding with CypherParameterValue."
```

---

## Task 6: DB API Integration

**Files:**
- Modify: `src/db/query.rs` (add `execute_cypher` / `execute_cypher_with_params`)
- Modify: `src/cypher/tests.rs` (add integration tests with real DB)

**Step 1: Write failing integration tests**

Add to `src/cypher/tests.rs`:

```rust
use crate::AletheiaDB;
use crate::core::property::PropertyMap as CorePropertyMap;

// === Integration Tests ===

#[test]
fn test_execute_cypher_simple() {
    let db = AletheiaDB::new().unwrap();
    let mut props = CorePropertyMap::new();
    props.set("name", "Alice");
    db.create_node("Person", props).unwrap();

    let results = db.execute_cypher("MATCH (n:Person) RETURN n").unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_property_filter() {
    let db = AletheiaDB::new().unwrap();
    let mut props1 = CorePropertyMap::new();
    props1.set("name", "Alice");
    db.create_node("Person", props1).unwrap();
    let mut props2 = CorePropertyMap::new();
    props2.set("name", "Bob");
    db.create_node("Person", props2).unwrap();

    let results = db.execute_cypher(
        "MATCH (n:Person {name: 'Alice'}) RETURN n"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_traversal() {
    let db = AletheiaDB::new().unwrap();
    let mut props_a = CorePropertyMap::new();
    props_a.set("name", "Alice");
    let alice = db.create_node("Person", props_a).unwrap();

    let mut props_b = CorePropertyMap::new();
    props_b.set("name", "Bob");
    let bob = db.create_node("Person", props_b).unwrap();

    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new()).unwrap();

    let results = db.execute_cypher(
        "MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_limit() {
    let db = AletheiaDB::new().unwrap();
    for i in 0..10 {
        let mut props = CorePropertyMap::new();
        props.set("name", format!("Person{}", i));
        db.create_node("Person", props).unwrap();
    }

    let results = db.execute_cypher(
        "MATCH (n:Person) RETURN n LIMIT 5"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_execute_cypher_with_params() {
    let db = AletheiaDB::new().unwrap();
    let mut props = CorePropertyMap::new();
    props.set("name", "Alice");
    db.create_node("Person", props).unwrap();

    use std::collections::HashMap;
    use crate::cypher::CypherParameterValue;
    let mut params = HashMap::new();
    params.insert("name".to_string(), CypherParameterValue::String("Alice".into()));

    let results = db.execute_cypher_with_params(
        "MATCH (n:Person {name: $name}) RETURN n",
        params,
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_execute_cypher_parse_error() {
    let db = AletheiaDB::new().unwrap();
    let result = db.execute_cypher("NOT VALID CYPHER");
    assert!(result.is_err());
}
```

**Step 2: Add DB methods**

In `src/db/query.rs`, add behind `#[cfg(feature = "cypher")]`:

```rust
#[cfg(feature = "cypher")]
impl AletheiaDB {
    /// Execute a Cypher query string.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let results = db.execute_cypher("MATCH (n:Person) RETURN n")?;
    /// ```
    pub fn execute_cypher(&self, query_string: &str) -> Result<QueryResults> {
        let query = crate::cypher::parse_cypher(query_string)
            .map_err(|e| crate::core::error::Error::Query(
                crate::core::error::QueryError::ParseError(e.to_string())
            ))?;
        self.execute_query(query)
    }

    /// Execute a Cypher query string with parameter bindings.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::collections::HashMap;
    /// use aletheiadb::cypher::CypherParameterValue;
    ///
    /// let mut params = HashMap::new();
    /// params.insert("name".into(), CypherParameterValue::String("Alice".into()));
    /// let results = db.execute_cypher_with_params(
    ///     "MATCH (n:Person {name: $name}) RETURN n",
    ///     params,
    /// )?;
    /// ```
    pub fn execute_cypher_with_params(
        &self,
        query_string: &str,
        params: std::collections::HashMap<String, crate::cypher::CypherParameterValue>,
    ) -> Result<QueryResults> {
        let query = crate::cypher::parse_cypher_with_params(query_string, params)
            .map_err(|e| crate::core::error::Error::Query(
                crate::core::error::QueryError::ParseError(e.to_string())
            ))?;
        self.execute_query(query)
    }
}
```

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/db/query.rs src/cypher/tests.rs
git commit -m "feat(cypher): add db.execute_cypher() API (#312)

Database-level Cypher execution with parameter binding support.
Full integration tests with real database operations."
```

---

## Task 7: Temporal Extensions

**Files:**
- Modify: `src/cypher/parser.rs` (add temporal clause parsing)
- Modify: `src/cypher/converter.rs` (add temporal → TemporalContext conversion)
- Modify: `src/cypher/tests.rs` (add temporal tests)

**Step 1: Write failing temporal tests**

```rust
// === Temporal Tests ===

#[test]
fn test_parse_as_of_timestamp() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00Z' RETURN n"
    ).unwrap();
    if let CypherStatement::Match { temporal: Some(t), .. } = ast {
        assert!(matches!(t, CypherTemporal::AsOfTimestamp(_)));
    } else {
        panic!("Expected temporal clause");
    }
}

#[test]
fn test_parse_as_of_valid_time() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) AS OF VALID_TIME '2024-01-15' RETURN n"
    ).unwrap();
    if let CypherStatement::Match { temporal: Some(t), .. } = ast {
        assert!(matches!(t, CypherTemporal::AsOfValidTime(_)));
    }
}

#[test]
fn test_parse_as_of_system_time() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) FOR SYSTEM_TIME AS OF '2024-01-15' RETURN n"
    ).unwrap();
    if let CypherStatement::Match { temporal: Some(t), .. } = ast {
        assert!(matches!(t, CypherTemporal::AsOfSystemTime(_)));
    }
}

#[test]
fn test_parse_bitemporal() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) AS OF VALID_TIME '2024-01-01' AS OF SYSTEM_TIME '2024-06-15' RETURN n"
    ).unwrap();
    if let CypherStatement::Match { temporal: Some(t), .. } = ast {
        assert!(matches!(t, CypherTemporal::BiTemporal { .. }));
    }
}

#[test]
fn test_parse_between() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) BETWEEN '2024-01-01' AND '2024-12-31' RETURN n"
    ).unwrap();
    if let CypherStatement::Match { temporal: Some(t), .. } = ast {
        assert!(matches!(t, CypherTemporal::Between { .. }));
    }
}

#[test]
fn test_convert_temporal_as_of() {
    let query = parse_cypher(
        "MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00Z' RETURN n"
    ).unwrap();
    // Query should have temporal_context set
    assert!(query.temporal_context.is_some());
}
```

**Step 2: Implement temporal parsing and conversion**

Parser additions:
- After MATCH pattern, check for `AS OF`, `FOR SYSTEM_TIME`, or `BETWEEN`
- `AS OF TIMESTAMP 'time'` → `CypherTemporal::AsOfTimestamp(time)`
- `AS OF VALID_TIME 'time'` → `CypherTemporal::AsOfValidTime(time)`
- `FOR SYSTEM_TIME AS OF 'time'` → `CypherTemporal::AsOfSystemTime(time)`
- `BETWEEN 'start' AND 'end'` → `CypherTemporal::Between { start, end }`

Converter additions:
- Parse timestamp strings using same logic as SQL module (see `src/sql/temporal_parser.rs` `parse_timestamp` function):
  - ISO 8601: `2024-01-15T10:00:00Z`
  - Date only: `2024-01-15` (midnight UTC)
  - Unix microseconds: `1705315200000000`
- `CypherTemporal::AsOfTimestamp(t)` → Set `query.temporal_context = Some(TemporalContext::as_of(timestamp, timestamp))`
- `CypherTemporal::AsOfValidTime(t)` + `AsOfSystemTime(s)` → Bi-temporal context
- `CypherTemporal::Between` → Set `query.ops` includes `QueryOp::Between { time_range }`

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/cypher/parser.rs src/cypher/converter.rs src/cypher/tests.rs
git commit -m "feat(cypher): add temporal extensions (#312)

AS OF TIMESTAMP, AS OF VALID_TIME, AS OF SYSTEM_TIME, bi-temporal,
and BETWEEN time range queries. Multi-format timestamp parsing."
```

---

## Task 8: Vector Extensions

**Files:**
- Modify: `src/cypher/parser.rs` (add vector function parsing)
- Modify: `src/cypher/converter.rs` (add vector → QueryOp conversion)
- Modify: `src/cypher/tests.rs` (add vector tests)

**Step 1: Write failing vector tests**

```rust
// === Vector Tests ===

#[test]
fn test_parse_vector_similarity_in_order_by() {
    let ast = CypherParser::parse(
        "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, $query) DESC LIMIT 10"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert_eq!(return_clause.order_by.len(), 1);
        assert!(matches!(
            &return_clause.order_by[0].expr,
            CypherExpr::FunctionCall { name, .. } if name == "vector.similarity"
        ));
    }
}

#[test]
fn test_parse_vector_cosine() {
    let ast = CypherParser::parse(
        "MATCH (d:Document) RETURN d, vector.cosine(d.embedding, $query) AS score ORDER BY score DESC"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        assert!(return_clause.items.len() >= 2);
    }
}

#[test]
fn test_convert_vector_rank() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    let emb: Arc<[f32]> = Arc::from([0.1f32, 0.2, 0.3].as_slice());
    params.insert("query".to_string(), CypherParameterValue::Embedding(emb));

    let query = parse_cypher_with_params(
        "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, $query) DESC LIMIT 10",
        params,
    ).unwrap();
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::RankBySimilarity { .. })));
}

#[test]
fn test_convert_hybrid_traverse_then_rank() {
    use std::collections::HashMap;
    let mut params = HashMap::new();
    let emb: Arc<[f32]> = Arc::from([0.1f32, 0.2, 0.3].as_slice());
    params.insert("targetEmbedding".to_string(), CypherParameterValue::Embedding(emb));

    let query = parse_cypher_with_params(
        "MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b ORDER BY vector.similarity(b.embedding, $targetEmbedding) DESC LIMIT 10",
        params,
    ).unwrap();
    // Should have both traversal and vector ranking
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::TraverseOut { .. })));
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::RankBySimilarity { .. })));
}
```

**Step 2: Implement vector extensions**

Parser additions:
- Function calls: `identifier.identifier(args)` → `CypherExpr::FunctionCall`
- Recognized functions: `vector.similarity`, `vector.cosine`, `vector.euclidean`
- Vector literal arrays in parameters: `[0.1, 0.2, 0.3]`

Converter additions:
- When ORDER BY contains `vector.similarity(prop, $param)` with DESC + LIMIT:
  → Replace with `QueryOp::RankBySimilarity { embedding, top_k, property_key }`
- `vector.cosine` → `DistanceMetric::Cosine`
- `vector.euclidean` → `DistanceMetric::Euclidean`
- Extract property key from first arg (e.g., `d.embedding` → `"embedding"`)
- Resolve embedding from second arg (parameter or literal)

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/cypher/parser.rs src/cypher/converter.rs src/cypher/tests.rs
git commit -m "feat(cypher): add vector extensions (#312)

vector.similarity(), vector.cosine(), vector.euclidean() functions.
ORDER BY vector.similarity() DESC LIMIT k → RankBySimilarity optimization.
Hybrid graph+vector queries supported."
```

---

## Task 9: WITH Clause & Advanced Features

**Files:**
- Modify: `src/cypher/parser.rs` (WITH clause, OPTIONAL MATCH, aggregations)
- Modify: `src/cypher/converter.rs` (WITH → query chaining, aggregations → QueryOp)
- Modify: `src/cypher/tests.rs` (advanced feature tests)

**Step 1: Write failing tests**

```rust
// === Advanced Feature Tests ===

#[test]
fn test_parse_with_clause() {
    let ast = CypherParser::parse(
        "MATCH (a:Person)-[:KNOWS]->(b) \
         WITH b, vector.cosine(b.embedding, $emb) AS similarity \
         WHERE similarity > 0.7 \
         RETURN b.name, similarity \
         ORDER BY similarity DESC LIMIT 10"
    ).unwrap();
    if let CypherStatement::Match { with_clauses, .. } = ast {
        assert_eq!(with_clauses.len(), 1);
    }
}

#[test]
fn test_parse_optional_match() {
    let ast = CypherParser::parse(
        "MATCH (n:Person) RETURN n"
    ).unwrap();
    if let CypherStatement::Match { optional, .. } = ast {
        assert!(!optional);
    }
    // Note: OPTIONAL MATCH parsing — future tests
}

#[test]
fn test_parse_count_aggregation() {
    let ast = CypherParser::parse(
        "MATCH (n:Person)-[:KNOWS]->(m) RETURN count(m)"
    ).unwrap();
    if let CypherStatement::Match { return_clause, .. } = ast {
        match &return_clause.items[0] {
            CypherReturnItem::Expression { expr, .. } => {
                assert!(matches!(expr, CypherExpr::FunctionCall { name, .. } if name == "count"));
            }
            _ => panic!("Expected function call"),
        }
    }
}

#[test]
fn test_parse_multiple_patterns() {
    let ast = CypherParser::parse(
        "MATCH (a:Person), (b:Person) WHERE a.name = 'Alice' AND b.name = 'Bob' RETURN a, b"
    ).unwrap();
    if let CypherStatement::Match { pattern, .. } = ast {
        assert_eq!(pattern.len(), 2);
    }
}

#[test]
fn test_convert_full_hybrid() {
    // "Who were the Doctor's companions in 2010 most similar to Rose?"
    use std::collections::HashMap;
    let mut params = HashMap::new();
    let emb: Arc<[f32]> = Arc::from([0.1f32, 0.2, 0.3].as_slice());
    params.insert("roseEmbedding".to_string(), CypherParameterValue::Embedding(emb));

    let query = parse_cypher_with_params(
        "MATCH (doctor:TimeLords {name: 'David Tennant'})-[:COMPANION]->(companion) \
         AS OF TIMESTAMP '2010-06-15T00:00:00Z' \
         RETURN companion \
         ORDER BY vector.similarity(companion.embedding, $roseEmbedding) DESC \
         LIMIT 10",
        params,
    ).unwrap();

    // Should have temporal context
    assert!(query.temporal_context.is_some());
    // Should have scan + filter + traverse + rank + limit
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::ScanNodes { .. })));
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::TraverseOut { .. })));
    assert!(query.ops.iter().any(|op| matches!(op, QueryOp::RankBySimilarity { .. })));
}
```

**Step 2: Implement WITH and advanced features**

Parser:
- WITH clause: `WITH items [WHERE expr]` between MATCH and RETURN
- Multiple patterns: `MATCH pattern1, pattern2`
- Function calls in RETURN: `count(n)`, `collect(n.name)`, `avg(n.age)`

Converter:
- WITH clause: Currently map to intermediate filter/projection ops
  (Full WITH semantics with variable scoping is complex — implement the common patterns:
  WITH x, expr AS alias WHERE filter → additional Filter + Project ops)
- `count()` → `QueryOp::Count`
- Multiple patterns → Multiple scan+filter chains (cartesian product semantics deferred; first pattern is primary)

**Step 3: Run tests, verify, commit**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
git add src/cypher/parser.rs src/cypher/converter.rs src/cypher/tests.rs
git commit -m "feat(cypher): add WITH clause and advanced features (#312)

WITH clause for query chaining, multiple patterns, count() aggregation,
and full hybrid query support (graph + temporal + vector)."
```

---

## Task 10: Comprehensive Integration Tests

**Files:**
- Modify: `src/cypher/tests.rs` (add comprehensive integration test suite)

**Step 1: Write end-to-end tests**

```rust
// === Comprehensive Integration Tests ===

#[test]
fn test_e2e_multi_hop_traversal() {
    let db = AletheiaDB::new().unwrap();
    // Create: Alice -> Bob -> Charlie
    let alice = db.create_node("Person", {
        let mut p = CorePropertyMap::new(); p.set("name", "Alice"); p
    }).unwrap();
    let bob = db.create_node("Person", {
        let mut p = CorePropertyMap::new(); p.set("name", "Bob"); p
    }).unwrap();
    let charlie = db.create_node("Person", {
        let mut p = CorePropertyMap::new(); p.set("name", "Charlie"); p
    }).unwrap();
    db.create_edge(alice, bob, "KNOWS", CorePropertyMap::new()).unwrap();
    db.create_edge(bob, charlie, "KNOWS", CorePropertyMap::new()).unwrap();

    // 2-hop traversal
    let results = db.execute_cypher(
        "MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..2]->(b) RETURN b"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert!(rows.len() >= 2); // Bob + Charlie
}

#[test]
fn test_e2e_where_complex() {
    let db = AletheiaDB::new().unwrap();
    for (name, age) in [("Alice", 30), ("Bob", 25), ("Charlie", 35)] {
        let mut p = CorePropertyMap::new();
        p.set("name", name);
        p.set("age", age as i64);
        db.create_node("Person", p).unwrap();
    }

    let results = db.execute_cypher(
        "MATCH (n:Person) WHERE n.age > 26 AND n.age < 34 RETURN n"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1); // Only Alice (age 30)
}

#[test]
fn test_e2e_order_skip_limit() {
    let db = AletheiaDB::new().unwrap();
    for i in 0..20 {
        let mut p = CorePropertyMap::new();
        p.set("name", format!("P{:02}", i));
        p.set("rank", i as i64);
        db.create_node("Item", p).unwrap();
    }

    let results = db.execute_cypher(
        "MATCH (n:Item) RETURN n ORDER BY n.rank DESC SKIP 5 LIMIT 5"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_e2e_bidirectional() {
    let db = AletheiaDB::new().unwrap();
    let a = db.create_node("Person", {
        let mut p = CorePropertyMap::new(); p.set("name", "A"); p
    }).unwrap();
    let b = db.create_node("Person", {
        let mut p = CorePropertyMap::new(); p.set("name", "B"); p
    }).unwrap();
    db.create_edge(a, b, "FRIEND", CorePropertyMap::new()).unwrap();

    // Bidirectional from B should find A
    let results = db.execute_cypher(
        "MATCH (x:Person {name: 'B'})-[:FRIEND]-(y) RETURN y"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_e2e_no_results() {
    let db = AletheiaDB::new().unwrap();
    let results = db.execute_cypher(
        "MATCH (n:NonexistentLabel) RETURN n"
    ).unwrap();
    let rows: Vec<_> = results.collect();
    assert_eq!(rows.len(), 0);
}
```

**Step 2: Run the full test suite and verify**

```bash
cargo test --features cypher -- cypher::tests
cargo clippy --all-features -- -D warnings && cargo fmt --all
```

**Step 3: Commit**

```bash
git add src/cypher/tests.rs
git commit -m "test(cypher): comprehensive integration test suite (#312)

Multi-hop traversals, complex WHERE predicates, ORDER BY + SKIP + LIMIT,
bidirectional patterns, edge cases, and end-to-end database integration."
```

---

## Task 11: Documentation & Final Cleanup

**Files:**
- Modify: `CLAUDE.md` (add Cypher section to features)
- Verify all doc comments are complete

**Step 1: Update CLAUDE.md**

Add a new section under "## Major Features" for the Cypher query language. Include the feature flag, quick start example, and supported syntax summary.

**Step 2: Final verification**

```bash
# Full quality gate
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --features cypher
cargo test  # Ensure no regression without feature
cargo doc --features cypher --no-deps  # Verify docs build
```

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(cypher): add Cypher query language documentation (#312)

Feature documentation in CLAUDE.md, doc comments on all public API items."
```

---

## Summary: Operation Count by Phase

| Phase | Tasks | Commits | Key Deliverables |
|-------|-------|---------|-----------------|
| **1: Foundation** | 1-2 | 2 | Feature flag, error types, module scaffold, AST types |
| **2: Parser** | 3-4 | 2 | Lexer (tokenizer), recursive descent parser |
| **3: Core** | 5-6 | 2 | AST→Query converter, `db.execute_cypher()` API |
| **4: Temporal** | 7 | 1 | AS OF, FOR SYSTEM_TIME, BETWEEN |
| **5: Vector** | 8 | 1 | vector.similarity/cosine/euclidean, hybrid queries |
| **6: Advanced** | 9 | 1 | WITH clause, aggregations, multiple patterns |
| **7: Polish** | 10-11 | 2 | Integration tests, documentation |

**Total: ~11 tasks, ~11 commits, ~7 files created, ~2 files modified**

## Testing Strategy

- **Unit tests**: Each module (lexer, parser, converter) has isolated unit tests
- **Integration tests**: Full database round-trip tests in `src/cypher/tests.rs`
- **Error tests**: Invalid syntax, missing clauses, unknown parameters
- **Edge cases**: Empty results, bidirectional traversals, multi-hop paths, parameter binding

## Risk Mitigation

- **No external dependencies**: Hand-written parser, zero compile-time impact when feature is off
- **Shared IR**: All Cypher queries compile to the same `Query`/`QueryOp` as AQL and SQL — existing planner and executor are reused unchanged
- **Feature-gated**: Zero cost when `cypher` feature is not enabled
- **Incremental**: Each task is independently testable and committable
