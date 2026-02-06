//! Cypher Query Language module.
//!
//! This module implements support for the Cypher query language.
//! It includes a hand-written recursive descent parser (no external dependencies).
//!
//! # Feature Flag
//!
//! Cypher support is gated behind the `cypher` Cargo feature on the `aletheiadb`
//! crate. To enable it, add the feature in your `Cargo.toml`:
//!
//! ```toml
//! aletheiadb = { version = "0.x", features = ["cypher"] }
//! ```
//!
//! Or enable it on the command line:
//!
//! ```sh
//! cargo test -p aletheiadb --features cypher
//! cargo run  -p aletheiadb --features cypher
//! ```
//!
//! # Architecture
//!
//! The Cypher query pipeline follows these phases:
//!
//! ```text
//! Cypher String
//!   → [Lexer]      (crate::cypher::lexer)
//!   → Tokens
//!   → [Parser]     (crate::cypher::parser)
//!   → AST          (crate::cypher::ast)
//!   → [Transformer](crate::cypher::transform)
//!   → QueryOp
//!   → Results
//! ```
//!
//! * **Lexer**: tokenizes the input Cypher string into a stream of tokens.
//! * **Parser**: consumes tokens and produces an abstract syntax tree (AST)
//!   representing the Cypher query.
//! * **Transformer**: converts the AST into AletheiaDB query operations
//!   (`QueryOp`) that can be executed by the engine.
//!
//! # Quick Start
//!
//! The most common way to execute Cypher is by combining a query string with a
//! parameter map created using the [`params!`] macro.
//!
//! ```ignore
//! use aletheiadb::cypher::params;
//!
//! // A simple read-only Cypher query with a named parameter.
//! let query = "MATCH (p:Person { name: $name }) RETURN p";
//!
//! // Build a parameter map for the query.
//! let parameters = params! {
//!     "name" => "Alice",
//! };
//!
//! // Pseudo-code: execute the query against your database handle.
//! // db.cypher_with_params(query, parameters);
//! ```
//!
//! The same `params!` macro can be used in tests or benchmarks to conveniently
//! construct parameter maps:
//!
//! ```ignore
//! use aletheiadb::cypher::params;
//!
//! let p = params! {
//!     "name" => "Bob",
//!     "age" => 42,
//! };
//! ```
//!
//! # Supported Cypher Syntax
//!
//! This module focuses on a practical subset of the Cypher query language that
//! is sufficient for typical AletheiaDB workloads. In broad terms, the
//! following features are supported or targeted:
//!
//! * Basic **read queries** using `MATCH` and `RETURN`.
//! * Node and relationship patterns:
//!   * Nodes with optional labels and property maps, e.g. `(n:Person { name: $name })`.
//!   * Relationships with optional types and directions, e.g. `()-[r:KNOWS]->()`.
//! * Simple boolean predicates in `WHERE` clauses (comparisons, conjunction, disjunction).
//! * Parameters (e.g. `$name`, `$age`) bound via the [`params!`] macro.
//! * Basic expressions in `RETURN` (variables, property access, aliases).
//!
//! More advanced Cypher constructs (updates, complex aggregation, procedures,
//! etc.) may be partially supported, experimental, or rejected during parsing or
//! transformation. Consult the individual submodules (`lexer`, `parser`,
//! `ast`, and `transform`) for the current, detailed behavior as the
//! implementation evolves.

pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;
#[cfg(test)]
mod tests;
pub mod transform;

/// Macro for creating a parameter map for Cypher queries.
///
/// This is similar to the `properties!` macro but designed for query parameters.
/// It creates a `PropertyMap` which can be passed to `cypher_with_params`.
///
/// # Example
///
/// ```ignore
/// use aletheiadb::params;
///
/// let p = params! {
///     "name" => "Alice",
///     "age" => 30,
/// };
/// ```
#[macro_export]
macro_rules! params {
    ($($key:expr => $value:expr),* $(,)?) => {
        {
            let mut builder = $crate::core::property::PropertyMapBuilder::new();
            $(
                builder = builder.insert($key, $value);
            )*
            builder.build()
        }
    };
}
