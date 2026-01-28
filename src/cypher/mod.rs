//! Cypher Query Language module.
//!
//! This module implements support for the Cypher query language.
//! It includes a hand-written recursive descent parser (no external dependencies).

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
/// use gallifreydb::cypher::params;
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
