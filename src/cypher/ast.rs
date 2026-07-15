//! Cypher Query Language Abstract Syntax Tree (AST)
//!
//! This module defines the AST types produced by the Cypher parser. Every type
//! in this module derives [`Debug`], [`Clone`], and [`PartialEq`] so that ASTs
//! can be inspected, duplicated, and compared in tests.
//!
//! # Structure
//!
//! ```text
//! CypherStatement
//!   ├── CypherPattern          (graph pattern: nodes + relationships)
//!   │     └── CypherPatternElement
//!   │           ├── CypherNodePattern
//!   │           └── CypherRelPattern
//!   ├── CypherExpr             (WHERE / filter expressions)
//!   ├── CypherReturn           (projection: items, ordering, pagination)
//!   ├── CypherTemporal         (bi-temporal qualifiers)
//!   └── CypherWith             (intermediate projection)
//! ```

use std::sync::Arc;

// ---------------------------------------------------------------------------
// Top-level statement
// ---------------------------------------------------------------------------

/// A complete Cypher statement ready for planning and execution.
///
/// Reading (`MATCH`) and write (`CREATE` / `MERGE` / `SET` / `DELETE`)
/// statements are supported; further clause types are added as new variants in
/// future phases.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherStatement {
    /// A `MATCH` (or `OPTIONAL MATCH`) statement that reads from the graph.
    Match {
        /// Whether this is an `OPTIONAL MATCH` (returns nulls for unmatched patterns).
        optional: bool,
        /// One or more comma-separated graph patterns to match.
        pattern: Vec<CypherPattern>,
        /// An optional `WHERE` clause that filters matched results.
        where_clause: Option<CypherExpr>,
        /// The `RETURN` clause that defines output projection.
        return_clause: CypherReturn,
        /// An optional temporal qualifier (e.g., `AS OF TIMESTAMP ...`).
        temporal: Option<CypherTemporal>,
        /// Zero or more intermediate `WITH` projections.
        with_clauses: Vec<CypherWith>,
        /// Zero or more subsequent `OPTIONAL MATCH` clauses.
        ///
        /// Source ordering relative to `with_clauses` is preserved via
        /// [`CypherOptionalMatch::preceding_withs`].
        optional_matches: Vec<CypherOptionalMatch>,
    },

    /// A standalone `UNWIND <list> AS <var> RETURN ...` statement (Issue #559).
    ///
    /// `UNWIND` expands a list value into one row per element, binding each
    /// element to `variable`. This variant models the *standalone* form only
    /// (an `UNWIND` that is not preceded by `MATCH`/`WITH` and does not feed a
    /// subsequent graph pattern); it is executed directly by the dedicated
    /// Cypher UNWIND runtime rather than lowered into the graph query pipeline.
    ///
    /// openCypher list semantics apply: an empty list and the `null` value both
    /// expand to zero rows.
    Unwind {
        /// The list-valued source expression. Supported forms are a list
        /// literal (`[...]`), a parameter reference (`$list`), or the `null`
        /// literal; other (row-context-dependent) expressions are rejected at
        /// execution time.
        source: CypherExpr,
        /// The variable each list element is bound to (the `AS <var>` target).
        variable: String,
        /// The trailing `RETURN` projection (with optional `DISTINCT`,
        /// `ORDER BY`, `SKIP`, `LIMIT`).
        return_clause: CypherReturn,
    },

    /// `EXPLAIN <statement>` -- return the query plan for the wrapped statement
    /// **without executing it** (Issue #562).
    ///
    /// The inner statement is any ordinary readable statement (a `MATCH`,
    /// optionally with a leading temporal clause). A nested/duplicate prefix
    /// (`EXPLAIN EXPLAIN`, `EXPLAIN PROFILE`) is rejected at parse time, so the
    /// boxed inner is never itself an `Explain`/`Profile`.
    Explain(Box<CypherStatement>),

    /// `PROFILE <statement>` -- **execute** the wrapped statement and return its
    /// plan annotated with per-operator executed statistics (row counts and
    /// timing) (Issue #562).
    ///
    /// Same nesting rules as [`CypherStatement::Explain`].
    Profile(Box<CypherStatement>),

    /// A write statement (Issue #560): `CREATE` / `SET` / `DELETE` /
    /// `DETACH DELETE`, optionally preceded by a reading `MATCH` and followed by
    /// a `RETURN`.
    ///
    /// Write statements are **not** lowered into the read-only [`Query`] IR;
    /// they are executed directly against the database's native write APIs (so
    /// each mutation records the correct bi-temporal version). They are reachable
    /// only through `AletheiaDB::execute_cypher` / `execute_cypher_with_params`;
    /// the MCP `query` tool rejects every mutating clause *before* the parser
    /// runs (`crate::query::read_only::detect_mutating_clause`), so this variant
    /// never executes through that read-only surface.
    ///
    /// [`Query`]: crate::query::Query
    Write(CypherWriteStatement),
}

/// A complete Cypher write statement (Issue #560).
///
/// Grammar (v1):
///
/// ```text
/// write_stmt := [reading] write_clause+ [RETURN ...]
/// reading    := MATCH pattern_list [WHERE expr]
/// write_clause := CREATE pattern_list
///               | SET set_item (',' set_item)*
///               | [DETACH] DELETE variable (',' variable)*
///               | MERGE pattern (ON CREATE SET set_item (',' set_item)*
///                               | ON MATCH SET set_item (',' set_item)*)*
/// set_item   := variable '.' property '=' value
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CypherWriteStatement {
    /// An optional leading reading clause (`MATCH ... [WHERE ...]`) whose matched
    /// rows drive the write clauses. `None` for a statement that opens directly
    /// with `CREATE`.
    pub reading: Option<CypherReadingClause>,
    /// One or more write clauses, applied in source order per matched row.
    pub clauses: Vec<CypherWriteClause>,
    /// An optional trailing `RETURN` projecting the affected entities.
    pub return_clause: Option<CypherReturn>,
}

/// The reading (`MATCH ... [WHERE ...]`) part of a write statement.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherReadingClause {
    /// One or more comma-separated graph patterns to match.
    pub pattern: Vec<CypherPattern>,
    /// An optional `WHERE` clause filtering matched rows.
    pub where_clause: Option<CypherExpr>,
}

/// A single write clause within a [`CypherWriteStatement`].
#[derive(Debug, Clone, PartialEq)]
pub enum CypherWriteClause {
    /// `CREATE <pattern_list>` -- create the described nodes and relationships.
    Create(Vec<CypherPattern>),
    /// `SET <set_item> (',' <set_item>)*` -- assign properties on bound entities.
    Set(Vec<CypherSetItem>),
    /// `[DETACH] DELETE <variable> (',' <variable>)*` -- delete bound entities.
    Delete {
        /// Whether this is a `DETACH DELETE` (cascade-remove connected edges).
        detach: bool,
        /// The variables to delete (nodes or relationships).
        targets: Vec<String>,
    },
    /// `MERGE <pattern> [ON CREATE SET ...] [ON MATCH SET ...]` (Issue #3548) --
    /// match the pattern if it already exists (per openCypher whole-pattern
    /// semantics), otherwise create the entire pattern. `ON CREATE SET` applies
    /// only on the create branch; `ON MATCH SET` only on the match branch.
    Merge {
        /// The single graph pattern to match-or-create.
        pattern: CypherPattern,
        /// Assignments applied only when the pattern is created.
        on_create: Vec<CypherSetItem>,
        /// Assignments applied only when the pattern is matched.
        on_match: Vec<CypherSetItem>,
    },
}

/// A single `SET n.prop = value` assignment.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherSetItem {
    /// The variable whose property is being set.
    pub variable: String,
    /// The property key to assign.
    pub property: String,
    /// The literal (or parameter) value assigned.
    pub value: CypherValue,
}

/// A subsequent `OPTIONAL MATCH` clause within a `MATCH` statement.
///
/// Per openCypher semantics the clause's `WHERE` is part of the optional
/// pattern itself: it participates in the matched/unmatched decision rather
/// than filtering rows afterwards.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherOptionalMatch {
    /// One or more comma-separated graph patterns to match optionally.
    pub pattern: Vec<CypherPattern>,
    /// An optional `WHERE` clause scoped to this optional pattern.
    pub where_clause: Option<CypherExpr>,
    /// Number of `WITH` clauses (in the statement's `with_clauses`) that
    /// appear before this clause in the query text. This preserves clause
    /// ordering so conversion can interleave `WITH ... WHERE` filters and
    /// optional patterns exactly as written.
    pub preceding_withs: usize,
}

// ---------------------------------------------------------------------------
// Graph patterns
// ---------------------------------------------------------------------------

/// A single graph pattern consisting of alternating node and relationship elements.
///
/// A valid pattern always starts and ends with a node. For example,
/// `(a:Person)-[:KNOWS]->(b:Person)` has three elements:
/// `[Node(a), Relationship(KNOWS), Node(b)]`.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherPattern {
    /// The ordered sequence of nodes and relationships that form this pattern.
    pub elements: Vec<CypherPatternElement>,
}

/// A single element inside a graph pattern -- either a node or a relationship.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherPatternElement {
    /// A node pattern, e.g. `(n:Person {name: 'Alice'})`.
    Node(CypherNodePattern),
    /// A relationship pattern, e.g. `-[:KNOWS]->`.
    Relationship(CypherRelPattern),
}

/// A node pattern within a `MATCH` clause.
///
/// Examples:
/// - `(n)` -- bare variable, no label or properties
/// - `(n:Person)` -- variable with label
/// - `(:Person {name: 'Alice'})` -- anonymous node with label and properties
#[derive(Debug, Clone, PartialEq)]
pub struct CypherNodePattern {
    /// An optional variable name bound to this node (e.g. `n` in `(n:Person)`).
    pub variable: Option<String>,
    /// Zero or more labels required on the node (e.g. `["Person"]`).
    pub labels: Vec<String>,
    /// Zero or more property key-value constraints (e.g. `{name: 'Alice'}`).
    pub properties: Vec<(String, CypherValue)>,
}

/// A relationship pattern within a `MATCH` clause.
///
/// Examples:
/// - `-[:KNOWS]->` -- outgoing, typed
/// - `<-[:FOLLOWS]-` -- incoming, typed
/// - `-[r:KNOWS|LIKES*1..3]->` -- outgoing, variable, multi-type, variable-length
#[derive(Debug, Clone, PartialEq)]
pub struct CypherRelPattern {
    /// An optional variable name bound to this relationship.
    pub variable: Option<String>,
    /// Zero or more relationship types (e.g. `["KNOWS", "LIKES"]`).
    pub rel_types: Vec<String>,
    /// The traversal direction of this relationship.
    pub direction: CypherDirection,
    /// An optional variable-length path specifier (e.g. `*1..3`).
    pub depth: Option<CypherDepth>,
    /// Zero or more property key-value constraints on the relationship.
    pub properties: Vec<(String, CypherValue)>,
}

/// The traversal direction of a relationship in a pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CypherDirection {
    /// Outgoing: `(a)-[]->(b)`.
    Outgoing,
    /// Incoming: `(a)<-[]-(b)`.
    Incoming,
    /// Bidirectional (either direction): `(a)-[]-(b)`.
    Both,
}

/// A variable-length path depth specifier.
///
/// Appears after `*` in a relationship pattern to indicate how many hops are
/// allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CypherDepth {
    /// `*` -- any number of hops (including zero).
    Unbounded,
    /// `*N` -- exactly N hops.
    Exact(usize),
    /// `*..M` -- at most M hops.
    Max(usize),
    /// `*N..` -- at least N hops.
    Min(usize),
    /// `*N..M` -- between N and M hops (inclusive).
    Range {
        /// Minimum number of hops.
        min: usize,
        /// Maximum number of hops.
        max: usize,
    },
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A literal value or parameter reference in a Cypher expression.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherValue {
    /// The `NULL` literal.
    Null,
    /// A boolean literal (`TRUE` / `FALSE`).
    Bool(bool),
    /// A 64-bit signed integer literal.
    Int(i64),
    /// A 64-bit floating-point literal.
    Float(f64),
    /// A string literal (single- or double-quoted).
    String(String),
    /// A parameter reference (e.g. `$name`). The string is the parameter name
    /// without the leading `$`.
    Parameter(String),
    /// A dense vector literal used for similarity search.
    Vector(Arc<[f32]>),
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// An expression node in the Cypher AST.
///
/// Expressions appear in `WHERE` clauses, `RETURN` items, and `ORDER BY` clauses.
/// They form a tree with operators as interior nodes and values / variables as leaves.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherExpr {
    /// A literal value (number, string, boolean, null, parameter, vector).
    Value(CypherValue),
    /// A bare variable reference (e.g. `n`).
    Variable(String),
    /// A property access expression (e.g. `n.name`).
    Property {
        /// The variable whose property is being accessed.
        variable: String,
        /// The property key name.
        property: String,
    },
    /// A comparison expression (e.g. `n.age > 18`).
    Comparison {
        /// The left-hand operand.
        left: Box<CypherExpr>,
        /// The comparison operator.
        op: CypherCompOp,
        /// The right-hand operand.
        right: Box<CypherExpr>,
    },
    /// Logical AND of two expressions.
    And(Box<CypherExpr>, Box<CypherExpr>),
    /// Logical OR of two expressions.
    Or(Box<CypherExpr>, Box<CypherExpr>),
    /// Logical NOT of an expression.
    Not(Box<CypherExpr>),
    /// `IS NULL` predicate.
    IsNull(Box<CypherExpr>),
    /// `IS NOT NULL` predicate.
    IsNotNull(Box<CypherExpr>),
    /// `IN [...]` list membership test.
    In {
        /// The expression to test for membership.
        expr: Box<CypherExpr>,
        /// The list of values to test against.
        values: Vec<CypherExpr>,
    },
    /// `CONTAINS 'substring'` string predicate.
    Contains {
        /// The expression whose string representation is tested.
        expr: Box<CypherExpr>,
        /// The substring to search for.
        substring: String,
    },
    /// `STARTS WITH 'prefix'` string predicate.
    StartsWith {
        /// The expression whose string representation is tested.
        expr: Box<CypherExpr>,
        /// The required prefix.
        prefix: String,
    },
    /// `ENDS WITH 'suffix'` string predicate.
    EndsWith {
        /// The expression whose string representation is tested.
        expr: Box<CypherExpr>,
        /// The required suffix.
        suffix: String,
    },
    /// A function call (e.g. `count(n)`, `avg(n.age)`).
    FunctionCall {
        /// The function name (case-insensitive at parse time).
        name: String,
        /// The arguments passed to the function.
        ///
        /// For the `count(*)` aggregate the single argument is
        /// [`CypherExpr::Star`].
        args: Vec<CypherExpr>,
        /// Whether the call used the `DISTINCT` quantifier, e.g.
        /// `count(DISTINCT n.dept)`. Only meaningful for aggregate functions;
        /// always `false` for ordinary/vector functions.
        distinct: bool,
    },
    /// The `*` wildcard argument, used only inside `count(*)`.
    Star,
    /// A parenthesized sub-expression used for grouping.
    Grouped(Box<CypherExpr>),
    /// A list literal, e.g. `[1, 2, 3]` or `[[1, 2], [3, 4]]`.
    ///
    /// Currently produced (and consumed) by the `UNWIND` source position; a
    /// list literal appearing where a scalar predicate operand is expected is
    /// rejected during conversion rather than silently mishandled.
    List(Vec<CypherExpr>),
}

/// A comparison operator in a Cypher expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CypherCompOp {
    /// `=` equality.
    Eq,
    /// `<>` or `!=` inequality.
    Ne,
    /// `<` less than.
    Lt,
    /// `<=` less than or equal.
    Le,
    /// `>` greater than.
    Gt,
    /// `>=` greater than or equal.
    Ge,
}

// ---------------------------------------------------------------------------
// RETURN clause
// ---------------------------------------------------------------------------

/// The `RETURN` clause of a Cypher statement.
///
/// Controls which data is projected out of the query, along with ordering,
/// deduplication, and pagination.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherReturn {
    /// Whether `DISTINCT` was specified to deduplicate results.
    pub distinct: bool,
    /// The items to return (variables, expressions, or `*`).
    pub items: Vec<CypherReturnItem>,
    /// Zero or more `ORDER BY` items that determine result ordering.
    pub order_by: Vec<CypherOrderItem>,
    /// An optional `SKIP n` offset for pagination.
    pub skip: Option<usize>,
    /// An optional `LIMIT n` cap on result count.
    pub limit: Option<usize>,
}

/// A single item in the `RETURN` clause.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherReturnItem {
    /// `RETURN *` -- return all bound variables.
    Star,
    /// A bare variable name (e.g. `RETURN n`).
    Variable(String),
    /// An arbitrary expression with an optional alias (e.g. `RETURN n.name AS name`).
    Expression {
        /// The expression to evaluate and return.
        expr: CypherExpr,
        /// An optional alias for the column (set via `AS`).
        alias: Option<String>,
    },
}

/// A single `ORDER BY` item specifying sort expression and direction.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherOrderItem {
    /// The expression to sort by.
    pub expr: CypherExpr,
    /// Whether to sort in descending order (`true` = DESC, `false` = ASC).
    pub descending: bool,
}

// ---------------------------------------------------------------------------
// Temporal extensions
// ---------------------------------------------------------------------------

/// A temporal qualifier for bi-temporal queries.
///
/// These extend standard Cypher with AletheiaDB's time-travel capabilities.
#[derive(Debug, Clone, PartialEq)]
pub enum CypherTemporal {
    /// `AS OF TIMESTAMP '...'` -- point-in-time query (defaults to valid time).
    AsOfTimestamp(String),
    /// `FOR VALID_TIME AS OF '...'` -- explicit valid-time point query.
    AsOfValidTime(String),
    /// `FOR SYSTEM_TIME AS OF '...'` -- explicit system/transaction-time point query.
    AsOfSystemTime(String),
    /// Bi-temporal query combining valid time and system time.
    BiTemporal {
        /// The valid-time timestamp.
        valid_time: String,
        /// The system/transaction-time timestamp.
        system_time: String,
    },
    /// `BETWEEN '...' AND '...'` -- time-range query.
    Between {
        /// The start of the time range (inclusive).
        start: String,
        /// The end of the time range (inclusive).
        end: String,
    },
}

// ---------------------------------------------------------------------------
// WITH clause
// ---------------------------------------------------------------------------

/// An intermediate `WITH` projection clause.
///
/// `WITH` acts like a sub-`RETURN` that pipes results into subsequent clauses,
/// optionally filtering with a `WHERE`. Per openCypher the clause body mirrors
/// a `RETURN` body -- `WITH [DISTINCT] items [ORDER BY ...] [SKIP n] [LIMIT n]`
/// -- followed by an optional trailing `WHERE` that filters the *projected*
/// rows (Issue #556).
#[derive(Debug, Clone, PartialEq)]
pub struct CypherWith {
    /// Whether `DISTINCT` was specified to deduplicate the projected rows.
    pub distinct: bool,
    /// The items to project through the `WITH`.
    pub items: Vec<CypherReturnItem>,
    /// Zero or more `ORDER BY` items ordering the projected rows.
    pub order_by: Vec<CypherOrderItem>,
    /// An optional `SKIP n` offset applied to the projected rows.
    pub skip: Option<usize>,
    /// An optional `LIMIT n` cap applied to the projected rows.
    pub limit: Option<usize>,
    /// An optional `WHERE` clause that filters after projection.
    pub where_clause: Option<CypherExpr>,
}
