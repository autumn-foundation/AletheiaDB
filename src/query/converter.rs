//! AST to IR Converter
//!
//! This module converts parsed AQL (Aletheia Query Language) queries from their
//! Abstract Syntax Tree (AST) representation into the internal `Query` type that
//! can be executed by the query planner and executor.
//!
//! # Architecture
//!
//! The query processing pipeline in AletheiaDB follows this flow:
//!
//! ```text
//! AQL String → [Parser] → AST → [Converter] → Query → [Planner] → PhysicalPlan → [Executor] → Results
//! ```
//!
//! This module handles the **Converter** step, bridging the gap between the
//! human-readable AST and the internal query representation.
//!
//! # Quick Start
//!
//! The simplest way to execute a AQL query is using the [`parse_query`] function:
//!
//! ```rust
//! use aletheiadb::query::{parse_query, QueryPlanner};
//! use aletheiadb::query::planner::Statistics;
//! use aletheiadb::storage::CurrentStorage;
//! use std::sync::Arc;
//!
//! // Parse and convert a AQL query
//! let query = parse_query("MATCH (n:Person) WHERE n.age > 21 RETURN n LIMIT 10")
//!     .expect("Failed to parse query");
//!
//! // Create planner and execute
//! let storage = Arc::new(CurrentStorage::new());
//! let stats = Arc::new(Statistics::default());
//! let planner = QueryPlanner::new(stats, storage);
//! let plan = planner.plan(query).expect("Failed to plan query");
//! ```
//!
//! # Using Parameters
//!
//! For queries with dynamic values, use parameterized queries to prevent injection
//! and improve performance through query caching:
//!
//! ```rust
//! use aletheiadb::query::{parse_query_with_params, ParameterValue};
//! use aletheiadb::query::ir::PredicateValue;
//! use std::collections::HashMap;
//! use std::sync::Arc;
//!
//! // Create parameter bindings
//! let mut params = HashMap::new();
//! params.insert(
//!     "min_age".to_string(),
//!     ParameterValue::Value(PredicateValue::Int(21)),
//! );
//!
//! // Parse with parameters (not yet fully supported in WHERE clause)
//! // For vector search with embedding parameters:
//! let mut vector_params = HashMap::new();
//! vector_params.insert(
//!     "embedding".to_string(),
//!     ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
//! );
//!
//! let query = parse_query_with_params(
//!     "SIMILAR TO $embedding LIMIT 10",
//!     vector_params,
//! ).expect("Failed to parse query");
//! ```
//!
//! # Supported Query Features
//!
//! ## Graph Pattern Matching (MATCH)
//!
//! ```text
//! MATCH (n:Person)                           -- Node with label
//! MATCH (n:Person)-[:KNOWS]->(m)             -- Outgoing relationship
//! MATCH (n)<-[:FOLLOWS]-(m)                  -- Incoming relationship
//! MATCH (n)-[:FRIENDS]-(m)                   -- Bidirectional
//! MATCH (n)-[:KNOWS*1..3]->(m)               -- Variable-length path
//! ```
//!
//! ## Vector Search
//!
//! ```text
//! SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10        -- k-NN search with literal
//! SIMILAR TO $embedding LIMIT 10             -- k-NN with parameter
//! FIND SIMILAR TO ($node_id) LIMIT 5         -- Find similar to existing node
//! MATCH (n) RANK BY SIMILARITY TO [...] TOP 5 -- Rank existing results
//! ```
//!
//! ## Temporal Queries
//!
//! ```text
//! AS OF 1704067200000000 MATCH (n) ...       -- Point-in-time (microseconds)
//! AS OF 1704067200000000, 1704153600000000 MATCH ... -- Valid time, tx time
//! BETWEEN 1704067200000000 AND 1704153600000000 MATCH ... -- Time range
//! ```
//!
//! ## Predicates (WHERE clause)
//!
//! ```text
//! WHERE n.age > 21                           -- Comparison
//! WHERE n.name = 'Alice'                     -- Equality
//! WHERE n.status IN ['active', 'pending']   -- IN list
//! WHERE n.bio CONTAINS 'engineer'            -- String contains
//! WHERE n.name STARTS WITH 'A'               -- String prefix
//! WHERE n.email ENDS WITH '.com'             -- String suffix
//! WHERE EXISTS(n.email)                      -- Property exists
//! WHERE n.deleted IS NULL                    -- NULL check
//! WHERE n.age > 21 AND n.active = true       -- Logical AND
//! WHERE n.role = 'admin' OR n.role = 'mod'   -- Logical OR
//! WHERE NOT n.banned = true                  -- Logical NOT
//! ```
//!
//! ## Result Modifiers
//!
//! ```text
//! RETURN n                                   -- Return nodes
//! RETURN DISTINCT n                          -- Deduplicate
//! RETURN n.name, n.age                       -- Project properties
//! SKIP 10 LIMIT 20                           -- Pagination
//! ```
//!
//! # Manual Conversion
//!
//! For more control, use [`AstConverter`] directly:
//!
//! ```rust
//! use aletheiadb::query::{Parser, AstConverter, ParameterValue};
//! use aletheiadb::core::NodeId;
//!
//! // Parse the query
//! let ast = Parser::parse("FIND SIMILAR TO ($node) LIMIT 5")
//!     .expect("Parse failed");
//!
//! // Create converter with parameters
//! let mut converter = AstConverter::new();
//! converter.bind("node", ParameterValue::NodeId(NodeId::new(42).unwrap()));
//!
//! // Convert to Query
//! let query = converter.convert(&ast).expect("Conversion failed");
//! ```
//!
//! # Error Handling
//!
//! The converter returns detailed errors for:
//!
//! - **Missing parameters**: When a `$param` reference has no binding
//! - **Type mismatches**: When a parameter has the wrong type (e.g., NodeId vs Embedding)
//! - **Invalid timestamps**: When timestamp strings can't be parsed
//! - **Syntax errors**: Propagated from the parser
//!
//! ```rust
//! use aletheiadb::query::parse_query;
//!
//! // Missing parameter error
//! let result = parse_query("SIMILAR TO $missing LIMIT 10");
//! assert!(result.is_err());
//! ```
//!
//! # Performance Considerations
//!
//! - **Reuse converters**: Create one [`AstConverter`] and reuse it for multiple
//!   queries with the same parameters
//! - **Parameterize queries**: Parameterized queries can be cached and reused
//! - **Avoid large IN lists**: Large `IN [...]` clauses are converted to sequential
//!   value checks; consider using joins for very large lists

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::NodeId;
use crate::core::error::{Error, QueryError, Result};
use crate::core::temporal::{TimeRange, Timestamp, time};
use crate::index::vector::DistanceMetric;

use super::ast::{
    ComparisonOp, DepthSpec, EmbeddingRef, Expression, NodePattern, NodeRef, OrderClause, Pattern,
    PatternElement, PredicateExpr, PropertyValue, QueryAst, RelationshipDirection,
    RelationshipPattern, ReturnClause, SourceClause, TemporalClause, TimestampLiteral,
};
use super::builder::Query;
use super::ir::{Predicate, PredicateValue, QueryOp, SortKey, TraversalDepth};
use super::plan::{QueryHints, TemporalContext};

/// Converts a parsed [`QueryAst`] into a [`Query`] that can be executed.
///
/// The `AstConverter` is the bridge between the parser output (AST) and the
/// query execution engine. It transforms the tree-structured AST into a
/// sequence of [`QueryOp`] operations that the planner can optimize.
///
/// # Conversion Process
///
/// The converter processes the AST in this order:
///
/// 1. **Temporal context**: `AS OF` or `BETWEEN` clauses → [`TemporalContext`]
/// 2. **Source clause**: `MATCH` patterns or vector search → source [`QueryOp`]s
/// 3. **WHERE clause**: Predicates → [`QueryOp::Filter`]
/// 4. **RANK clause**: Similarity ranking → [`QueryOp::RankBySimilarity`]
/// 5. **RETURN clause**: Projections → [`QueryOp::Project`], [`QueryOp::Distinct`]
/// 6. **Modifiers**: `SKIP`, `LIMIT` → [`QueryOp::Skip`], [`QueryOp::Limit`]
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::{Parser, AstConverter};
///
/// let ast = Parser::parse("MATCH (n:Person) WHERE n.age > 21 RETURN n").unwrap();
/// let converter = AstConverter::new();
/// let query = converter.convert(&ast).unwrap();
///
/// // The query now contains: ScanNodes, Filter, Project operations
/// ```
///
/// # Parameter Binding
///
/// Use [`bind`](Self::bind) to set parameter values before conversion:
///
/// ```rust
/// use aletheiadb::query::{Parser, AstConverter, ParameterValue};
/// use std::sync::Arc;
///
/// let ast = Parser::parse("SIMILAR TO $embedding LIMIT 10").unwrap();
///
/// let mut converter = AstConverter::new();
/// converter.bind(
///     "embedding",
///     ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
/// );
///
/// let query = converter.convert(&ast).unwrap();
/// ```
pub struct AstConverter {
    /// Parameter bindings for the query.
    ///
    /// Parameters are referenced in AQL using `$name` syntax and must be
    /// bound before conversion.
    parameters: HashMap<String, ParameterValue>,
}

/// Parameter values that can be bound to a query.
///
/// AQL queries can reference parameters using the `$name` syntax. These
/// parameters must be bound to actual values before the query can be
/// converted and executed.
///
/// # Parameter Types
///
/// | Type | AQL Usage | Example |
/// |------|-----------|---------|
/// | `NodeId` | `FIND SIMILAR TO ($node)` | Reference to a specific node |
/// | `Embedding` | `SIMILAR TO $embedding` | Vector for k-NN search |
/// | `Value` | `WHERE n.age > $min_age` | Scalar comparison value |
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::ParameterValue;
/// use aletheiadb::query::ir::PredicateValue;
/// use aletheiadb::core::NodeId;
/// use std::sync::Arc;
///
/// // Node ID parameter
/// let node_param = ParameterValue::NodeId(NodeId::new(42).unwrap());
///
/// // Embedding parameter (384-dimensional vector)
/// let embedding_param = ParameterValue::Embedding(
///     Arc::from(vec![0.0f32; 384].as_slice())
/// );
///
/// // Scalar value parameter
/// let age_param = ParameterValue::Value(PredicateValue::Int(21));
/// let name_param = ParameterValue::Value(PredicateValue::String("Alice".to_string()));
/// ```
#[derive(Debug, Clone)]
pub enum ParameterValue {
    /// A node ID parameter for `FIND SIMILAR TO ($node)` queries.
    ///
    /// Used when searching for nodes similar to an existing node.
    NodeId(NodeId),

    /// An embedding vector parameter for `SIMILAR TO $embedding` queries.
    ///
    /// The vector dimensions must match the configured index dimensions.
    Embedding(Arc<[f32]>),

    /// A scalar value parameter for use in predicates.
    ///
    /// Supports all predicate value types: null, bool, int, float, string.
    Value(PredicateValue),
}

impl AstConverter {
    /// Create a new converter with no parameter bindings.
    ///
    /// Use this when the query has no parameters, or when you'll bind
    /// parameters later using [`bind`](Self::bind).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::query::AstConverter;
    ///
    /// let converter = AstConverter::new();
    /// ```
    pub fn new() -> Self {
        AstConverter {
            parameters: HashMap::new(),
        }
    }

    /// Create a converter with pre-populated parameter bindings.
    ///
    /// This is useful when parameters are collected from an external source
    /// (e.g., HTTP request, configuration file).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::query::{AstConverter, ParameterValue};
    /// use aletheiadb::query::ir::PredicateValue;
    /// use std::collections::HashMap;
    ///
    /// let mut params = HashMap::new();
    /// params.insert("limit".to_string(), ParameterValue::Value(PredicateValue::Int(100)));
    ///
    /// let converter = AstConverter::with_parameters(params);
    /// ```
    pub fn with_parameters(parameters: HashMap<String, ParameterValue>) -> Self {
        AstConverter { parameters }
    }

    /// Bind a parameter value by name.
    ///
    /// Parameters in AQL are referenced with the `$name` syntax. This method
    /// associates a name with a concrete value.
    ///
    /// Returns `&mut Self` for method chaining.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::query::{AstConverter, ParameterValue};
    /// use aletheiadb::core::NodeId;
    /// use std::sync::Arc;
    ///
    /// let mut converter = AstConverter::new();
    /// converter
    ///     .bind("node_id", ParameterValue::NodeId(NodeId::new(1).unwrap()))
    ///     .bind("embedding", ParameterValue::Embedding(Arc::from([0.1f32; 384].as_slice())));
    /// ```
    pub fn bind(&mut self, name: impl Into<String>, value: ParameterValue) -> &mut Self {
        self.parameters.insert(name.into(), value);
        self
    }

    /// Convert an AST to a Query.
    ///
    /// This is the main conversion method. It processes all parts of the AST
    /// and produces a [`Query`] containing a sequence of [`QueryOp`] operations.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A referenced parameter (`$name`) is not bound
    /// - A parameter has the wrong type for its usage
    /// - A timestamp string cannot be parsed
    /// - The time range in `BETWEEN` is invalid (start > end)
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::query::{Parser, AstConverter};
    ///
    /// let ast = Parser::parse("MATCH (n:Person) RETURN n LIMIT 10").unwrap();
    /// let converter = AstConverter::new();
    ///
    /// match converter.convert(&ast) {
    ///     Ok(query) => println!("Query converted successfully"),
    ///     Err(e) => eprintln!("Conversion failed: {}", e),
    /// }
    /// ```
    pub fn convert(&self, ast: &QueryAst) -> Result<Query> {
        // OPTIMIZATION: Pre-allocate vector to avoid reallocations.
        // Most queries produce at least a few operations (Scan, Return, etc.), so 8 is a conservative start.
        let mut ops = Vec::with_capacity(8);
        let hints = QueryHints::default();

        // 1. Convert temporal clause to context
        let temporal_context = self.convert_temporal(&ast.temporal)?;

        // 2. Convert source clause (MATCH or vector search)
        self.convert_source(&ast.source, &mut ops)?;

        // 3. Convert WHERE clause to filter operations
        if let Some(ref where_clause) = ast.where_clause {
            self.convert_where_clause(where_clause, &mut ops)?;
        }

        // 4. Convert RANK BY SIMILARITY clause
        if let Some(ref rank) = ast.rank {
            self.convert_rank_clause(rank, &mut ops)?;
        }

        // 5. Convert RETURN clause to projection
        if let Some(ref return_clause) = ast.return_clause {
            self.convert_return_clause_ops(return_clause, &mut ops)?;
        }

        // 6. Convert ORDER BY clause
        if let Some(ref order_clause) = ast.order {
            self.convert_order_clause(order_clause, &mut ops)?;
        }

        // 7. Convert SKIP and LIMIT
        self.convert_pagination(ast.skip, ast.limit, &mut ops);

        Ok(Query {
            ops,
            temporal_context,
            hints,
        })
    }

    fn convert_where_clause(
        &self,
        where_clause: &super::ast::WhereClause,
        ops: &mut Vec<QueryOp>,
    ) -> Result<()> {
        let predicate = self.convert_predicate(&where_clause.predicate)?;
        ops.push(QueryOp::Filter(predicate));
        Ok(())
    }

    fn convert_rank_clause(
        &self,
        rank: &super::ast::RankClause,
        ops: &mut Vec<QueryOp>,
    ) -> Result<()> {
        let embedding = self.resolve_embedding(&rank.embedding)?;
        ops.push(QueryOp::RankBySimilarity {
            embedding,
            top_k: rank.top_k,
            property_key: None,
        });
        Ok(())
    }

    fn convert_return_clause_ops(
        &self,
        return_clause: &ReturnClause,
        ops: &mut Vec<QueryOp>,
    ) -> Result<()> {
        let projection = self.convert_return(return_clause)?;
        if !projection.is_empty() {
            ops.push(QueryOp::Project(projection));
        }
        if return_clause.distinct {
            ops.push(QueryOp::Distinct);
        }
        Ok(())
    }

    fn convert_pagination(
        &self,
        skip: Option<usize>,
        limit: Option<usize>,
        ops: &mut Vec<QueryOp>,
    ) {
        if let Some(skip) = skip {
            ops.push(QueryOp::Skip(skip));
        }
        if let Some(limit) = limit {
            ops.push(QueryOp::Limit(limit));
        }
    }

    /// Convert temporal clause to TemporalContext.
    fn convert_temporal(
        &self,
        temporal: &Option<TemporalClause>,
    ) -> Result<Option<TemporalContext>> {
        match temporal {
            None => Ok(None),
            Some(TemporalClause::AsOf {
                valid_time,
                transaction_time,
            }) => {
                let vt = self.convert_timestamp(valid_time)?;
                let tt = match transaction_time {
                    Some(t) => self.convert_timestamp(t)?,
                    None => time::now(),
                };
                Ok(Some(TemporalContext::as_of(vt, tt)))
            }
            Some(TemporalClause::Between { start, end }) => {
                let start_ts = self.convert_timestamp(start)?;
                let end_ts = self.convert_timestamp(end)?;
                let range = TimeRange::new(start_ts, end_ts).map_err(|e| {
                    Error::Query(QueryError::InvalidParameter {
                        parameter: "time_range".to_string(),
                        reason: format!("Invalid time range: {}", e),
                    })
                })?;
                Ok(Some(TemporalContext::valid_time_between(range)))
            }
        }
    }

    /// Convert a timestamp literal to a Timestamp.
    ///
    /// Accepts:
    /// - Integer: treated as microseconds since Unix epoch
    /// - String containing an integer: parsed as microseconds
    ///
    /// Note: ISO 8601 string parsing requires the `mcp-server` feature.
    fn convert_timestamp(&self, ts: &TimestampLiteral) -> Result<Timestamp> {
        match ts {
            // Treat integer as microseconds (consistent with the database's internal format)
            TimestampLiteral::Integer(micros) => Ok(Timestamp::from(*micros)),
            TimestampLiteral::String(s) => {
                // Try parsing as microseconds since epoch
                if let Ok(micros) = s.parse::<i64>() {
                    return Ok(Timestamp::from(micros));
                }

                Err(Error::Query(QueryError::InvalidParameter {
                    parameter: "timestamp".to_string(),
                    reason: format!(
                        "Invalid timestamp '{}'. Expected microseconds since epoch.",
                        s
                    ),
                }))
            }
        }
    }

    /// Convert source clause to query operations.
    fn convert_source(&self, source: &SourceClause, ops: &mut Vec<QueryOp>) -> Result<()> {
        match source {
            SourceClause::Match(patterns) => {
                self.convert_patterns(patterns, ops)?;
            }
            SourceClause::VectorSearch {
                embedding,
                metric,
                limit,
            } => {
                let emb = self.resolve_embedding(embedding)?;
                ops.push(QueryOp::VectorSearch {
                    embedding: emb,
                    k: *limit,
                    metric: metric.unwrap_or(DistanceMetric::Cosine),
                    property_key: None,
                });
            }
            SourceClause::FindSimilar { node_ref, limit } => {
                let node_id = self.resolve_node_ref(node_ref)?;
                ops.push(QueryOp::SimilarTo {
                    source_node: node_id,
                    k: *limit,
                    property_key: None,
                    label_filter: None,
                });
            }
        }
        Ok(())
    }

    /// Convert MATCH patterns to query operations.
    fn convert_patterns(&self, patterns: &[Pattern], ops: &mut Vec<QueryOp>) -> Result<()> {
        for pattern in patterns {
            self.convert_pattern(pattern, ops)?;
        }
        Ok(())
    }

    /// Convert a single pattern to query operations.
    fn convert_pattern(&self, pattern: &Pattern, ops: &mut Vec<QueryOp>) -> Result<()> {
        let mut is_first = true;

        for element in &pattern.elements {
            match element {
                PatternElement::Node(node) => {
                    if is_first {
                        // First node - this is the starting point
                        self.convert_node_pattern(node, ops)?;
                        is_first = false;
                    }
                    // Subsequent nodes are targets of traversals, handled in relationship conversion
                }
                PatternElement::Relationship(rel) => {
                    self.convert_relationship_pattern(rel, ops)?;
                }
            }
        }
        Ok(())
    }

    /// Convert a node pattern to query operations.
    fn convert_node_pattern(&self, node: &NodePattern, ops: &mut Vec<QueryOp>) -> Result<()> {
        let mut optimized_start = false;

        // Check for inline property filters that might specify a node ID
        if let Some(ref props) = node.properties {
            for (key, value) in props {
                if key == "id" {
                    let node_id_res = match value {
                        PropertyValue::Int(id) => Some(NodeId::new(*id as u64)),
                        PropertyValue::Parameter(name) => {
                            // If parameter resolves to a NodeId (or Int), use StartNode optimization
                            self.parameters
                                .get(name)
                                .and_then(|param_val| match param_val {
                                    ParameterValue::NodeId(id) => Some(Ok(*id)),
                                    ParameterValue::Value(PredicateValue::Int(id)) => {
                                        Some(NodeId::new(*id as u64))
                                    }
                                    _ => None, // Other types don't support StartNode optimization
                                })
                        }
                        _ => None,
                    };

                    if let Some(res) = node_id_res {
                        ops.push(QueryOp::StartNode(res?));
                        optimized_start = true;
                    }
                }
                if optimized_start {
                    break;
                }
            }
        }

        // Otherwise, scan by label or all nodes
        if !optimized_start {
            ops.push(QueryOp::ScanNodes {
                label: node.label.clone(),
            });
        } else if let Some(ref label) = node.label {
            // Security: If we optimized to StartNode, we MUST still apply the label check
            // if one was specified (e.g. MATCH (n:Secret {id: 1})).
            ops.push(QueryOp::FilterLabel(label.clone()));
        }

        // Add filters for inline properties
        if let Some(ref props) = node.properties {
            for (key, value) in props {
                if key != "id" {
                    let pred_value = self.convert_property_value(value)?;
                    ops.push(QueryOp::Filter(Predicate::Eq {
                        key: key.clone(),
                        value: pred_value,
                    }));
                }
            }
        }

        Ok(())
    }

    /// Convert a relationship pattern to traversal operations.
    fn convert_relationship_pattern(
        &self,
        rel: &RelationshipPattern,
        ops: &mut Vec<QueryOp>,
    ) -> Result<()> {
        let depth = self.convert_depth_spec(&rel.depth);
        let label = rel.rel_type.clone();

        match rel.direction {
            RelationshipDirection::Outgoing => {
                ops.push(QueryOp::TraverseOut { label, depth });
            }
            RelationshipDirection::Incoming => {
                ops.push(QueryOp::TraverseIn { label, depth });
            }
            RelationshipDirection::Both => {
                ops.push(QueryOp::TraverseBoth { label, depth });
            }
        }

        Ok(())
    }

    /// Convert depth specification from AST to IR.
    fn convert_depth_spec(&self, depth: &Option<DepthSpec>) -> TraversalDepth {
        match depth {
            None => TraversalDepth::Exact(1),
            Some(DepthSpec::Exact(n)) => TraversalDepth::Exact(*n),
            Some(DepthSpec::Max(n)) => TraversalDepth::Max(*n),
            Some(DepthSpec::Range { min, max }) => TraversalDepth::Range {
                min: *min,
                max: *max,
            },
            Some(DepthSpec::Variable) => TraversalDepth::Variable,
        }
    }

    /// Convert a predicate expression to IR Predicate.
    fn convert_predicate(&self, expr: &PredicateExpr) -> Result<Predicate> {
        match expr {
            PredicateExpr::Comparison { left, op, right } => {
                self.convert_comparison(left, *op, right)
            }
            PredicateExpr::Exists(prop) => Ok(Predicate::Exists(prop.property.clone())),
            PredicateExpr::IsNull(prop) => Ok(Predicate::NotExists(prop.property.clone())),
            PredicateExpr::IsNotNull(prop) => Ok(Predicate::Exists(prop.property.clone())),
            PredicateExpr::Contains { .. }
            | PredicateExpr::StartsWith { .. }
            | PredicateExpr::EndsWith { .. } => self.convert_string_predicate(expr),
            PredicateExpr::In { property, values } => self.convert_in_predicate(property, values),
            PredicateExpr::And(_, _) | PredicateExpr::Or(_, _) | PredicateExpr::Not(_) => {
                self.convert_logic_predicate(expr)
            }
            PredicateExpr::Grouped(inner) => self.convert_predicate(inner),
        }
    }

    /// Convert logic predicates (AND, OR, NOT).
    fn convert_logic_predicate(&self, expr: &PredicateExpr) -> Result<Predicate> {
        match expr {
            PredicateExpr::And(left, right) => {
                let l = self.convert_predicate(left)?;
                let r = self.convert_predicate(right)?;
                Ok(l.and(r))
            }
            PredicateExpr::Or(left, right) => {
                let l = self.convert_predicate(left)?;
                let r = self.convert_predicate(right)?;
                Ok(l.or(r))
            }
            PredicateExpr::Not(inner) => {
                let p = self.convert_predicate(inner)?;
                Ok(!p)
            }
            _ => unreachable!("convert_logic_predicate called on non-logic expr"),
        }
    }

    /// Convert string predicates (CONTAINS, STARTS WITH, ENDS WITH).
    fn convert_string_predicate(&self, expr: &PredicateExpr) -> Result<Predicate> {
        match expr {
            PredicateExpr::Contains {
                property,
                substring,
            } => Ok(Predicate::Contains {
                key: property.property.clone(),
                substring: substring.clone(),
            }),
            PredicateExpr::StartsWith { property, prefix } => Ok(Predicate::StartsWith {
                key: property.property.clone(),
                prefix: prefix.clone(),
            }),
            PredicateExpr::EndsWith { property, suffix } => Ok(Predicate::EndsWith {
                key: property.property.clone(),
                suffix: suffix.clone(),
            }),
            _ => unreachable!("convert_string_predicate called on non-string expr"),
        }
    }

    /// Convert IN predicate.
    fn convert_in_predicate(
        &self,
        property: &super::ast::PropertyAccess,
        values: &[PropertyValue],
    ) -> Result<Predicate> {
        let pred_values: Result<Vec<PredicateValue>> = values
            .iter()
            .map(|v| self.convert_property_value(v))
            .collect();
        Ok(Predicate::In {
            key: property.property.clone(),
            values: pred_values?,
        })
    }

    /// Convert a comparison expression.
    fn convert_comparison(
        &self,
        left: &Expression,
        op: ComparisonOp,
        right: &Expression,
    ) -> Result<Predicate> {
        // Try left side as property
        if let Expression::Property(prop) = left {
            let key = prop.property.clone();
            let value = self.expression_to_predicate_value(right)?;

            return Ok(match op {
                ComparisonOp::Eq => Predicate::Eq { key, value },
                ComparisonOp::Ne => Predicate::Ne { key, value },
                ComparisonOp::Lt => Predicate::Lt { key, value },
                ComparisonOp::Le => Predicate::Lte { key, value },
                ComparisonOp::Gt => Predicate::Gt { key, value },
                ComparisonOp::Ge => Predicate::Gte { key, value },
            });
        }

        // Try right side as property (swap operands)
        if let Expression::Property(prop) = right {
            let key = prop.property.clone();
            let value = self.expression_to_predicate_value(left)?;

            // When swapping, we must flip inequalities
            return Ok(match op {
                ComparisonOp::Eq => Predicate::Eq { key, value }, // Symmetric
                ComparisonOp::Ne => Predicate::Ne { key, value }, // Symmetric
                ComparisonOp::Lt => Predicate::Gt { key, value }, // < becomes >
                ComparisonOp::Le => Predicate::Gte { key, value }, // <= becomes >=
                ComparisonOp::Gt => Predicate::Lt { key, value }, // > becomes <
                ComparisonOp::Ge => Predicate::Lte { key, value }, // >= becomes <=
            });
        }

        Err(Error::Query(QueryError::SyntaxError {
            message: "Comparison must involve a property access (e.g., n.age)".to_string(),
        }))
    }

    /// Convert an expression to a predicate value.
    fn expression_to_predicate_value(&self, expr: &Expression) -> Result<PredicateValue> {
        match expr {
            Expression::Literal(pv) => self.convert_property_value(pv),
            Expression::Parameter(name) => {
                if let Some(ParameterValue::Value(v)) = self.parameters.get(name) {
                    Ok(v.clone())
                } else {
                    Err(Error::Query(QueryError::InvalidParameter {
                        parameter: name.clone(),
                        reason: "not found or has wrong type".to_string(),
                    }))
                }
            }
            _ => Err(Error::Query(QueryError::SyntaxError {
                message: "Expected literal or parameter in comparison".to_string(),
            })),
        }
    }

    /// Convert a property value to a predicate value.
    fn convert_property_value(&self, value: &PropertyValue) -> Result<PredicateValue> {
        match value {
            PropertyValue::Null => Ok(PredicateValue::Null),
            PropertyValue::Bool(b) => Ok(PredicateValue::Bool(*b)),
            PropertyValue::Int(i) => Ok(PredicateValue::Int(*i)),
            PropertyValue::Float(f) => Ok(PredicateValue::Float(*f)),
            PropertyValue::String(s) => Ok(PredicateValue::String(s.clone())),
            PropertyValue::Parameter(name) => {
                if let Some(ParameterValue::Value(v)) = self.parameters.get(name) {
                    Ok(v.clone())
                } else {
                    Err(Error::Query(QueryError::InvalidParameter {
                        parameter: name.clone(),
                        reason: "not found or has wrong type".to_string(),
                    }))
                }
            }
        }
    }

    /// Convert RETURN clause to projection list.
    fn convert_return(&self, return_clause: &ReturnClause) -> Result<Vec<String>> {
        // OPTIMIZATION: Pre-allocate vector with exact size to avoid reallocations.
        let mut projections = Vec::with_capacity(return_clause.items.len());
        for item in &return_clause.items {
            match &item.expression {
                Expression::Property(prop) => {
                    projections.push(prop.property.clone());
                }
                Expression::Identifier(name) => {
                    // Bare variable (e.g., RETURN n) - store the variable name.
                    // The executor handles returning the full node for bare variables.
                    projections.push(name.clone());
                }
                _ => {
                    // Other expressions are computed at execution time
                }
            }
        }
        Ok(projections)
    }

    /// Convert ORDER BY clause to Sort operations.
    fn convert_order_clause(
        &self,
        order_clause: &OrderClause,
        ops: &mut Vec<QueryOp>,
    ) -> Result<()> {
        for item in &order_clause.items {
            let sort_key = match &item.expression {
                Expression::Property(prop) => SortKey::Property(prop.property.clone()),
                Expression::Identifier(name) => {
                    // Special identifiers for built-in sort keys
                    match name.as_str() {
                        "score" => SortKey::Score,
                        "timestamp" => SortKey::Timestamp,
                        _ => SortKey::Property(name.clone()),
                    }
                }
                _ => {
                    return Err(Error::Query(QueryError::SyntaxError {
                        message: "ORDER BY expression must be a property access or identifier"
                            .to_string(),
                    }));
                }
            };

            ops.push(QueryOp::Sort {
                key: sort_key,
                descending: item.descending,
            });
        }
        Ok(())
    }

    /// Resolve an embedding reference to an actual embedding vector.
    fn resolve_embedding(&self, emb_ref: &EmbeddingRef) -> Result<Arc<[f32]>> {
        match emb_ref {
            EmbeddingRef::Literal(arr) => Ok(arr.clone()),
            EmbeddingRef::Parameter(name) => {
                if let Some(ParameterValue::Embedding(emb)) = self.parameters.get(name) {
                    Ok(emb.clone())
                } else {
                    Err(Error::Query(QueryError::InvalidParameter {
                        parameter: name.clone(),
                        reason: "embedding parameter not found".to_string(),
                    }))
                }
            }
        }
    }

    /// Resolve a node reference to a NodeId.
    fn resolve_node_ref(&self, node_ref: &NodeRef) -> Result<NodeId> {
        match node_ref {
            NodeRef::Id(id) => Ok(NodeId::new(*id)?),
            NodeRef::Parameter(name) => {
                if let Some(ParameterValue::NodeId(id)) = self.parameters.get(name) {
                    Ok(*id)
                } else {
                    Err(Error::Query(QueryError::InvalidParameter {
                        parameter: name.clone(),
                        reason: "node ID parameter not found".to_string(),
                    }))
                }
            }
            NodeRef::Identifier(name) => Err(Error::Query(QueryError::InvalidParameter {
                parameter: name.clone(),
                reason: "variable node references require execution context".to_string(),
            })),
        }
    }
}

impl Default for AstConverter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a AQL query string and convert it to a Query.
///
/// This is the simplest way to convert a AQL string into an executable query.
/// It combines parsing and conversion in a single step.
///
/// # Arguments
///
/// * `gql` - A AQL query string (e.g., `"MATCH (n:Person) RETURN n"`)
///
/// # Returns
///
/// * `Ok(Query)` - The converted query ready for planning/execution
/// * `Err(Error)` - Parse error or conversion error
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::parse_query;
///
/// // Simple node scan
/// let query = parse_query("MATCH (n:Person) RETURN n").unwrap();
///
/// // Graph traversal with filter
/// let query = parse_query(
///     "MATCH (n:Person)-[:KNOWS]->(m:Person) WHERE m.age > 21 RETURN m"
/// ).unwrap();
///
/// // Vector search
/// let query = parse_query("SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10").unwrap();
///
/// // Temporal query
/// let query = parse_query("AS OF 1704067200000000 MATCH (n:Person) RETURN n").unwrap();
/// ```
///
/// # Errors
///
/// Returns an error for:
/// - Syntax errors in the AQL string
/// - Unbound parameters (use [`parse_query_with_params`] for parameterized queries)
///
/// ```rust
/// use aletheiadb::query::parse_query;
///
/// // Syntax error
/// let result = parse_query("MATCH (n:Person RETURN n"); // Missing closing paren
/// assert!(result.is_err());
///
/// // Unbound parameter
/// let result = parse_query("SIMILAR TO $embedding LIMIT 10");
/// assert!(result.is_err());
/// ```
pub fn parse_query(gql: &str) -> Result<Query> {
    let ast = super::parser::Parser::parse(gql).map_err(|e| {
        Error::Query(QueryError::SyntaxError {
            message: e.to_string(),
        })
    })?;
    let converter = AstConverter::new();
    converter.convert(&ast)
}

/// Parse a AQL query string with parameters and convert it to a Query.
///
/// Use this function when your query contains parameter references (`$name`).
/// Parameters allow dynamic values without string concatenation, which:
///
/// - Prevents injection vulnerabilities
/// - Enables query plan caching
/// - Provides type safety
///
/// # Arguments
///
/// * `gql` - A AQL query string with parameter references
/// * `params` - A map of parameter names to values
///
/// # Returns
///
/// * `Ok(Query)` - The converted query with parameters resolved
/// * `Err(Error)` - Parse error, conversion error, or missing parameter
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::{parse_query_with_params, ParameterValue};
/// use std::collections::HashMap;
/// use std::sync::Arc;
///
/// // Vector search with embedding parameter
/// let mut params = HashMap::new();
/// params.insert(
///     "embedding".to_string(),
///     ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
/// );
///
/// let query = parse_query_with_params(
///     "SIMILAR TO $embedding LIMIT 10",
///     params,
/// ).unwrap();
/// ```
///
/// # Parameter Types
///
/// Different query contexts require different parameter types:
///
/// | Context | Required Type | Example |
/// |---------|--------------|---------|
/// | `SIMILAR TO $param` | `Embedding` | `ParameterValue::Embedding(Arc::from(...))` |
/// | `FIND SIMILAR TO ($param)` | `NodeId` | `ParameterValue::NodeId(NodeId::new(42)?)` |
/// | `WHERE n.prop = $param` | `Value` | `ParameterValue::Value(PredicateValue::Int(21))` |
///
/// # Errors
///
/// Returns an error if:
/// - The AQL string has syntax errors
/// - A referenced parameter is not in the `params` map
/// - A parameter has the wrong type for its context
pub fn parse_query_with_params(
    gql: &str,
    params: HashMap<String, ParameterValue>,
) -> Result<Query> {
    let ast = super::parser::Parser::parse(gql).map_err(|e| {
        Error::Query(QueryError::SyntaxError {
            message: e.to_string(),
        })
    })?;
    let converter = AstConverter::with_parameters(params);
    converter.convert(&ast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parser::Parser;

    // ========================================================================
    // RED PHASE: Failing tests for basic conversion
    // ========================================================================

    #[test]
    fn test_convert_simple_match() {
        // MATCH (n:Person) RETURN n
        let ast = Parser::parse("MATCH (n:Person) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have ScanNodes with label "Person"
        assert!(!query.ops.is_empty());
        assert!(matches!(
            &query.ops[0],
            QueryOp::ScanNodes {
                label: Some(l)
            } if l == "Person"
        ));
    }

    #[test]
    fn test_convert_match_with_traversal() {
        // MATCH (n:Person)-[:KNOWS]->(m) RETURN m
        let ast = Parser::parse("MATCH (n:Person)-[:KNOWS]->(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have ScanNodes + TraverseOut
        assert!(query.ops.len() >= 2);
        assert!(matches!(
            &query.ops[0],
            QueryOp::ScanNodes { label: Some(l) } if l == "Person"
        ));
        assert!(matches!(
            &query.ops[1],
            QueryOp::TraverseOut {
                label: Some(l),
                depth: TraversalDepth::Exact(1)
            } if l == "KNOWS"
        ));
    }

    #[test]
    fn test_convert_match_with_where() {
        // MATCH (n:Person) WHERE n.age > 25 RETURN n
        let ast = Parser::parse("MATCH (n:Person) WHERE n.age > 25 RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have ScanNodes + Filter
        assert!(query.ops.len() >= 2);
        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Gt { key, .. } if key == "age"));
        }
    }

    #[test]
    fn test_convert_match_with_limit() {
        // MATCH (n:Person) RETURN n LIMIT 10
        let ast = Parser::parse("MATCH (n:Person) RETURN n LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have Limit operation
        let limit_op = query.ops.iter().find(|op| matches!(op, QueryOp::Limit(_)));
        assert!(limit_op.is_some());
        if let Some(QueryOp::Limit(n)) = limit_op {
            assert_eq!(*n, 10);
        }
    }

    #[test]
    fn test_convert_match_with_skip() {
        // MATCH (n:Person) RETURN n SKIP 5 LIMIT 10
        let ast = Parser::parse("MATCH (n:Person) RETURN n SKIP 5 LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have Skip operation
        let skip_op = query.ops.iter().find(|op| matches!(op, QueryOp::Skip(_)));
        assert!(skip_op.is_some());
        if let Some(QueryOp::Skip(n)) = skip_op {
            assert_eq!(*n, 5);
        }
    }

    #[test]
    fn test_convert_vector_search() {
        // SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10
        let ast = Parser::parse("SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have VectorSearch operation
        assert!(matches!(&query.ops[0], QueryOp::VectorSearch { k: 10, .. }));
    }

    #[test]
    fn test_convert_find_similar_with_parameter() {
        // FIND SIMILAR TO ($node_id) LIMIT 5
        let ast = Parser::parse("FIND SIMILAR TO ($node_id) LIMIT 5").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("node_id", ParameterValue::NodeId(NodeId::new(42).unwrap()));
        let query = converter.convert(&ast).unwrap();

        // Should have SimilarTo operation
        assert!(matches!(
            &query.ops[0],
            QueryOp::SimilarTo {
                source_node,
                k: 5,
                ..
            } if source_node.as_u64() == 42
        ));
    }

    #[test]
    fn test_convert_temporal_as_of() {
        // AS OF 1000 MATCH (n:Person) RETURN n
        let ast = Parser::parse("AS OF 1000 MATCH (n:Person) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have temporal context
        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.unwrap();
        let as_of_tuple = ctx.as_of_tuple();
        assert!(as_of_tuple.is_some());
        let (vt, _tt) = as_of_tuple.unwrap();
        // Timestamp is stored as microseconds, so 1000 is 1000 microseconds
        assert_eq!(vt.wallclock(), 1000);
    }

    #[test]
    fn test_convert_temporal_between() {
        // BETWEEN 1000 AND 2000 MATCH (n:Person) RETURN n
        let ast = Parser::parse("BETWEEN 1000 AND 2000 MATCH (n:Person) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should have temporal context with between
        assert!(query.temporal_context.is_some());
        let ctx = query.temporal_context.unwrap();
        assert!(ctx.valid_time_between.is_some());
    }

    #[test]
    fn test_convert_predicate_and() {
        // MATCH (n) WHERE n.a = 1 AND n.b = 2 RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.a = 1 AND n.b = 2 RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::And(_)));
        }
    }

    #[test]
    fn test_convert_predicate_or() {
        // MATCH (n) WHERE n.a = 1 OR n.b = 2 RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.a = 1 OR n.b = 2 RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Or(_)));
        }
    }

    #[test]
    fn test_convert_predicate_not() {
        // MATCH (n) WHERE NOT n.active = true RETURN n
        let ast = Parser::parse("MATCH (n) WHERE NOT n.active = true RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(pred, Predicate::Not(_)));
        }
    }

    #[test]
    fn test_convert_predicate_contains() {
        // MATCH (n) WHERE n.name CONTAINS 'test' RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.name CONTAINS 'test' RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::Contains { key, substring } if key == "name" && substring == "test"
            ));
        }
    }

    #[test]
    fn test_convert_predicate_starts_with() {
        // MATCH (n) WHERE n.name STARTS WITH 'Al' RETURN n
        let ast = Parser::parse("MATCH (n) WHERE n.name STARTS WITH 'Al' RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(matches!(
                pred,
                Predicate::StartsWith { key, prefix } if key == "name" && prefix == "Al"
            ));
        }
    }

    #[test]
    fn test_convert_predicate_in() {
        // MATCH (n) WHERE n.status IN ['active', 'pending'] RETURN n
        let ast =
            Parser::parse("MATCH (n) WHERE n.status IN ['active', 'pending'] RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let filter_op = query.ops.iter().find(|op| matches!(op, QueryOp::Filter(_)));
        assert!(filter_op.is_some());
        if let Some(QueryOp::Filter(pred)) = filter_op {
            assert!(
                matches!(pred, Predicate::In { key, values } if key == "status" && values.len() == 2)
            );
        }
    }

    #[test]
    fn test_convert_variable_length_traversal() {
        // MATCH (n)-[:KNOWS*1..3]->(m) RETURN m
        let ast = Parser::parse("MATCH (n)-[:KNOWS*1..3]->(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let traverse_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseOut { .. }));
        assert!(traverse_op.is_some());
        if let Some(QueryOp::TraverseOut { depth, .. }) = traverse_op {
            assert!(matches!(depth, TraversalDepth::Range { min: 1, max: 3 }));
        }
    }

    #[test]
    fn test_convert_incoming_traversal() {
        // MATCH (n)<-[:KNOWS]-(m) RETURN m
        let ast = Parser::parse("MATCH (n)<-[:KNOWS]-(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let traverse_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseIn { .. }));
        assert!(traverse_op.is_some());
    }

    #[test]
    fn test_convert_bidirectional_traversal() {
        // MATCH (n)-[:KNOWS]-(m) RETURN m
        let ast = Parser::parse("MATCH (n)-[:KNOWS]-(m) RETURN m").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let traverse_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::TraverseBoth { .. }));
        assert!(traverse_op.is_some());
    }

    #[test]
    fn test_convert_rank_by_similarity() {
        // MATCH (n:Document) RANK BY SIMILARITY TO [0.1, 0.2] TOP 5 RETURN n
        let ast =
            Parser::parse("MATCH (n:Document) RANK BY SIMILARITY TO [0.1, 0.2] TOP 5 RETURN n")
                .unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let rank_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::RankBySimilarity { .. }));
        assert!(rank_op.is_some());
        if let Some(QueryOp::RankBySimilarity { top_k, .. }) = rank_op {
            assert_eq!(*top_k, Some(5));
        }
    }

    #[test]
    fn test_convert_distinct() {
        // MATCH (n) RETURN DISTINCT n
        let ast = Parser::parse("MATCH (n) RETURN DISTINCT n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let distinct_op = query.ops.iter().find(|op| matches!(op, QueryOp::Distinct));
        assert!(distinct_op.is_some());
    }

    #[test]
    fn test_convert_with_embedding_parameter() {
        // SIMILAR TO $embedding LIMIT 10
        let ast = Parser::parse("SIMILAR TO $embedding LIMIT 10").unwrap();
        let mut converter = AstConverter::new();
        converter.bind(
            "embedding",
            ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
        );
        let query = converter.convert(&ast).unwrap();

        assert!(matches!(&query.ops[0], QueryOp::VectorSearch { k: 10, .. }));
    }

    #[test]
    fn test_convert_error_missing_parameter() {
        // SIMILAR TO $embedding LIMIT 10 (without binding)
        let ast = Parser::parse("SIMILAR TO $embedding LIMIT 10").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);

        assert!(result.is_err());
    }

    // ========================================================================
    // Convenience function tests
    // ========================================================================

    #[test]
    fn test_parse_query() {
        let query = super::parse_query("MATCH (n:Person) RETURN n").unwrap();
        assert!(!query.ops.is_empty());
    }

    #[test]
    fn test_parse_query_with_params() {
        use std::collections::HashMap;

        let mut params = HashMap::new();
        params.insert(
            "embedding".to_string(),
            ParameterValue::Embedding(Arc::from([0.1f32, 0.2, 0.3].as_slice())),
        );

        let query =
            super::parse_query_with_params("SIMILAR TO $embedding LIMIT 10", params).unwrap();
        assert!(matches!(&query.ops[0], QueryOp::VectorSearch { k: 10, .. }));
    }

    // ========================================================================
    // Planner integration tests
    // ========================================================================

    #[test]
    fn test_planner_integration_simple_match() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query = super::parse_query("MATCH (n:Person) RETURN n LIMIT 10").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query - should succeed
        let result = planner.plan(query);
        assert!(result.is_ok());

        let plan = result.unwrap();
        // Verify the plan has a valid root operation (not empty)
        assert!(!matches!(
            plan.root,
            crate::query::planner::PhysicalOp::Empty
        ));
    }

    #[test]
    fn test_planner_integration_with_traversal() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query = super::parse_query("MATCH (n:Person)-[:KNOWS]->(m:Person) RETURN m").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query
        let result = planner.plan(query);
        assert!(result.is_ok());
    }

    #[test]
    fn test_planner_integration_with_filter() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query =
            super::parse_query("MATCH (n:Person) WHERE n.age > 25 RETURN n LIMIT 10").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query
        let result = planner.plan(query);
        assert!(result.is_ok());
    }

    #[test]
    fn test_planner_integration_temporal() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert - temporal query
        let query = super::parse_query("AS OF 1000000 MATCH (n:Person) RETURN n").unwrap();
        assert!(query.temporal_context.is_some());

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query
        let result = planner.plan(query);
        assert!(result.is_ok());

        let plan = result.unwrap();
        // Temporal queries should include temporal context in the plan
        assert!(plan.is_temporal());
    }

    #[test]
    fn test_full_pipeline_parse_convert_plan() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Complex query with multiple operations
        let gql = "MATCH (n:Person)-[:KNOWS*1..3]->(m:Person) WHERE n.age > 21 AND m.active = true RETURN m LIMIT 100";

        // Parse
        let ast = Parser::parse(gql).unwrap();

        // Convert
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Verify conversion produced expected operations
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::ScanNodes { .. }))
        );
        assert!(
            query
                .ops
                .iter()
                .any(|op| matches!(op, QueryOp::TraverseOut { .. }))
        );
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Filter(_))));
        assert!(query.ops.iter().any(|op| matches!(op, QueryOp::Limit(100))));

        // Plan
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        let plan = planner.plan(query).unwrap();
        // Verify the plan has a valid root operation (not empty)
        assert!(!matches!(
            plan.root,
            crate::query::planner::PhysicalOp::Empty
        ));
    }

    // ========================================================================
    // ORDER BY conversion tests
    // ========================================================================

    #[test]
    fn test_convert_order_by_property() {
        // MATCH (n:Person) RETURN n ORDER BY n.age DESC
        let ast = Parser::parse("MATCH (n:Person) RETURN n ORDER BY n.age DESC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some(), "Expected Sort operation");
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(
                matches!(key, SortKey::Property(p) if p == "age"),
                "Expected property key 'age'"
            );
            assert!(*descending, "Expected descending order");
        }
    }

    #[test]
    fn test_convert_order_by_ascending() {
        // MATCH (n:Person) RETURN n ORDER BY n.name ASC
        let ast = Parser::parse("MATCH (n:Person) RETURN n ORDER BY n.name ASC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some(), "Expected Sort operation");
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(
                matches!(key, SortKey::Property(p) if p == "name"),
                "Expected property key 'name'"
            );
            assert!(!*descending, "Expected ascending order");
        }
    }

    #[test]
    fn test_convert_order_by_score() {
        // SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10 ORDER BY score DESC
        let ast = Parser::parse("SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10 ORDER BY score DESC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_op = query
            .ops
            .iter()
            .find(|op| matches!(op, QueryOp::Sort { .. }));
        assert!(sort_op.is_some(), "Expected Sort operation");
        if let Some(QueryOp::Sort { key, descending }) = sort_op {
            assert!(matches!(key, SortKey::Score), "Expected Score key");
            assert!(*descending, "Expected descending order");
        }
    }

    #[test]
    fn test_convert_order_by_multiple() {
        // MATCH (n) RETURN n ORDER BY n.age DESC, n.name ASC
        let ast = Parser::parse("MATCH (n) RETURN n ORDER BY n.age DESC, n.name ASC").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        let sort_ops: Vec<_> = query
            .ops
            .iter()
            .filter(|op| matches!(op, QueryOp::Sort { .. }))
            .collect();

        assert_eq!(sort_ops.len(), 2, "Expected 2 Sort operations");

        // First sort by age DESC
        if let QueryOp::Sort { key, descending } = sort_ops[0] {
            assert!(
                matches!(key, SortKey::Property(p) if p == "age"),
                "First sort should be by age"
            );
            assert!(*descending, "First sort should be descending");
        }

        // Second sort by name ASC
        if let QueryOp::Sort { key, descending } = sort_ops[1] {
            assert!(
                matches!(key, SortKey::Property(p) if p == "name"),
                "Second sort should be by name"
            );
            assert!(!*descending, "Second sort should be ascending");
        }
    }

    #[test]
    fn test_planner_integration_with_order_by() {
        use crate::query::planner::{QueryPlanner, Statistics};
        use crate::storage::CurrentStorage;
        use std::sync::Arc;

        // Parse and convert
        let query =
            super::parse_query("MATCH (n:Person) RETURN n ORDER BY n.age DESC LIMIT 10").unwrap();

        // Create storage and planner
        let storage = Arc::new(CurrentStorage::new());
        let stats = Arc::new(Statistics::default());
        let planner = QueryPlanner::new(stats, storage);

        // Plan the query - should succeed
        let result = planner.plan(query);
        assert!(result.is_ok(), "Planning should succeed");

        let plan = result.unwrap();
        assert!(!matches!(
            plan.root,
            crate::query::planner::PhysicalOp::Empty
        ));
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use crate::core::NodeId;
    use crate::query::parser::Parser;

    #[test]
    fn test_start_node_optimization_with_parameter() {
        // 🎯 Target: convert_node_pattern optimization for id lookup
        // 💣 Risk: Optimization missed when using parameters -> full scan -> slow
        // 🧪 Strategy: Bind a parameter for ID and check for QueryOp::StartNode

        let ast = Parser::parse("MATCH (n {id: $id}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::NodeId(NodeId::new(123).unwrap()));

        let query = converter.convert(&ast).unwrap();

        // Should optimize to StartNode(123)
        let has_start_node = query.ops.iter().any(|op| match op {
            QueryOp::StartNode(id) => id.as_u64() == 123,
            _ => false,
        });

        assert!(
            has_start_node,
            "Query should be optimized to use StartNode when id is a parameter. Ops: {:?}",
            query.ops
        );
    }

    #[test]
    fn test_comparison_asymmetry() {
        // 🎯 Target: convert_comparison
        // 💣 Risk: Inconvenient API (WHERE 1 = n.a fails)
        // 🧪 Strategy: Try WHERE value = property and assert success

        let ast = Parser::parse("MATCH (n) WHERE 1 = n.age RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast);

        assert!(
            query.is_ok(),
            "Comparison should be symmetric (value = property). Error: {:?}",
            query.err()
        );

        let query = query.unwrap();
        let has_filter = query.ops.iter().any(|op| match op {
            QueryOp::Filter(Predicate::Eq { key, .. }) => key == "age",
            _ => false,
        });
        assert!(has_filter, "Should produce equality filter for age");
    }

    #[test]
    fn test_invalid_node_id_in_match() {
        // 🎯 Target: convert_node_pattern with invalid ID
        // 💣 Risk: Undefined behavior or panic
        // 🧪 Strategy: Use negative ID (invalid for u64 NodeId)

        let ast = Parser::parse("MATCH (n {id: -1}) RETURN n").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);

        // Should handle gracefully (error or empty result), but definitely not panic
        // Currently expect error because NodeId::new fails
        assert!(result.is_err());
    }

    #[test]
    fn test_duplicate_property_keys() {
        // 🎯 Target: convert_node_pattern property handling
        // 💣 Risk: Ambiguous behavior
        // 🧪 Strategy: Use duplicate keys

        let ast = Parser::parse("MATCH (n {a: 1, a: 2}) RETURN n").unwrap();
        let converter = AstConverter::new();
        let query = converter.convert(&ast).unwrap();

        // Should produce two filters
        let filters = query
            .ops
            .iter()
            .filter(|op| matches!(op, QueryOp::Filter(_)))
            .count();
        // convert_node_pattern generates QueryOp::Filter for each property
        assert_eq!(filters, 2);
    }

    #[test]
    fn test_start_node_optimization_preserves_filters() {
        // 🎯 Target: convert_node_pattern optimization correctness
        // 💣 Risk: Optimization drops other filters (e.g. {id: 1, active: true} -> active ignored)
        // 🧪 Strategy: Bind parameter for ID, add another property, verify both StartNode and Filter present

        let ast = Parser::parse("MATCH (n {id: $id, active: true}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::NodeId(NodeId::new(123).unwrap()));

        let query = converter.convert(&ast).unwrap();

        let has_start_node = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::StartNode(_)));
        let has_filter = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::Filter(Predicate::Eq { key, .. }) if key == "active"));

        assert!(has_start_node, "Should use StartNode");
        assert!(has_filter, "Should preserve 'active' filter");
    }

    #[test]
    fn test_start_node_optimization_with_integer_parameter() {
        // 🎯 Target: convert_node_pattern optimization with Integer value
        // 💣 Risk: Integer parameters (common from JSON) not optimizing
        // 🧪 Strategy: Bind ParameterValue::Value(Int) instead of NodeId

        let ast = Parser::parse("MATCH (n {id: $id}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::Value(PredicateValue::Int(123)));

        let query = converter.convert(&ast).unwrap();

        let has_start_node = query.ops.iter().any(|op| match op {
            QueryOp::StartNode(id) => id.as_u64() == 123,
            _ => false,
        });
        assert!(has_start_node, "Should optimize Int parameter to StartNode");
    }

    #[test]
    fn test_symmetric_comparison_operators() {
        // 🎯 Target: convert_comparison operator flipping
        // 💣 Risk: Incorrect logic (e.g., < becomes > instead of >)
        // 🧪 Strategy: Test all inequalities in swapped position

        let cases = vec![
            (
                "10 = n.a",
                Predicate::Eq {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ),
            (
                "10 <> n.a",
                Predicate::Ne {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ),
            (
                "10 > n.a",
                Predicate::Lt {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 > a  => a < 10
            (
                "10 >= n.a",
                Predicate::Lte {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 >= a => a <= 10
            (
                "10 < n.a",
                Predicate::Gt {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 < a  => a > 10
            (
                "10 <= n.a",
                Predicate::Gte {
                    key: "a".to_string(),
                    value: PredicateValue::Int(10),
                },
            ), // 10 <= a => a >= 10
        ];

        for (query_str, expected) in cases {
            let full_query = format!("MATCH (n) WHERE {} RETURN n", query_str);
            let ast = Parser::parse(&full_query).unwrap();
            let converter = AstConverter::new();
            let query = converter.convert(&ast).unwrap();

            let found = query.ops.iter().any(|op| {
                if let QueryOp::Filter(pred) = op {
                    // Simple check for variant and value match.
                    // Note: Predicate doesn't derive PartialEq, so we format debug strings
                    format!("{:?}", pred) == format!("{:?}", expected)
                } else {
                    false
                }
            });
            assert!(
                found,
                "Failed to convert '{}'. Expected {:?}",
                query_str, expected
            );
        }
    }

    #[test]
    fn test_comparison_invalid_syntax() {
        // 🎯 Target: convert_comparison error path
        // 💣 Risk: Panic or incorrect behavior on invalid syntax
        // 🧪 Strategy: Compare two literals

        let ast = Parser::parse("MATCH (n) WHERE 1 = 1 RETURN n").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Comparison must involve a property")
        );
    }

    #[test]
    fn test_comparison_with_parameter_resolution() {
        // 🎯 Target: expression_to_predicate_value with parameter
        // 💣 Risk: Parameters not resolving in WHERE clause
        // 🧪 Strategy: Use parameter in WHERE

        let ast = Parser::parse("MATCH (n) WHERE n.age = $age RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("age", ParameterValue::Value(PredicateValue::Int(30)));
        let query = converter.convert(&ast).unwrap();

        let found = query.ops.iter().any(|op| {
            matches!(
                op,
                QueryOp::Filter(Predicate::Eq {
                    value: PredicateValue::Int(30),
                    ..
                })
            )
        });
        assert!(found, "Should resolve parameter in comparison");

        // Also test swapped with parameter
        let ast_swapped = Parser::parse("MATCH (n) WHERE $age = n.age RETURN n").unwrap();
        let query_swapped = converter.convert(&ast_swapped).unwrap();
        let found_swapped = query_swapped.ops.iter().any(|op| {
            matches!(
                op,
                QueryOp::Filter(Predicate::Eq {
                    value: PredicateValue::Int(30),
                    ..
                })
            )
        });
        assert!(
            found_swapped,
            "Should resolve parameter in swapped comparison"
        );
    }

    #[test]
    fn test_parameter_not_found_error() {
        // 🎯 Target: expression_to_predicate_value error path
        // 💣 Risk: Silent failure or panic on missing parameter
        // 🧪 Strategy: Use unbound parameter

        let ast = Parser::parse("MATCH (n) WHERE n.age = $missing RETURN n").unwrap();
        let converter = AstConverter::new();
        let result = converter.convert(&ast);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing"));
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_start_node_optimization_preserves_labels() {
        // 🎯 Target: convert_node_pattern optimization security
        // 💣 Risk: Optimization drops label check (e.g. MATCH (n:Secret {id: 1}) returns node 1 even if not Secret)
        // 🧪 Strategy: Bind parameter for ID, include label, verify both StartNode and FilterLabel present

        let ast = Parser::parse("MATCH (n:Secret {id: $id}) RETURN n").unwrap();
        let mut converter = AstConverter::new();
        converter.bind("id", ParameterValue::NodeId(NodeId::new(123).unwrap()));

        let query = converter.convert(&ast).unwrap();

        let has_start_node = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::StartNode(_)));
        let has_label_filter = query
            .ops
            .iter()
            .any(|op| matches!(op, QueryOp::FilterLabel(label) if label == "Secret"));

        assert!(has_start_node, "Should use StartNode");
        assert!(has_label_filter, "Should preserve 'Secret' label check");
    }
}
