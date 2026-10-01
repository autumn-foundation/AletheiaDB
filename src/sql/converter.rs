//! SQL to QueryOp Converter.
//!
//! This module converts parsed SQL AST from sqlparser-rs into AletheiaDB's
//! internal Query representation (QueryOp operations).

use std::collections::HashMap;
use std::sync::Arc;

use sqlparser::ast::{
    BinaryOperator, Expr, OrderByExpr, Query as SqlQuery, SelectItem, SetExpr, Statement,
    TableFactor, TableWithJoins, Value,
};

use crate::index::vector::DistanceMetric;
use crate::query::builder::Query;
use crate::query::ir::{Predicate, PredicateValue, QueryOp, SortKey};
use crate::query::plan::QueryHints;

use super::error::SqlError;
use super::match_parser;
use super::parser::SqlParser;
use super::temporal_parser;
use super::vector_parser::{self, EmbeddingRef, VectorOp};

/// Default k for SIMILAR_TO threshold-based search when no explicit limit is given.
/// This is an approximation: we over-fetch candidates then filter by threshold.
const SIMILAR_TO_DEFAULT_K: usize = 100;

/// Parameter values that can be bound to SQL queries.
#[derive(Debug, Clone)]
pub enum SqlParameterValue {
    /// Scalar value (string, int, float, bool)
    Scalar(PredicateValue),
    /// Vector embedding for k-NN search
    Embedding(Arc<[f32]>),
}

/// Converter from SQL AST to AletheiaDB Query.
///
/// # Example
///
/// ```rust
/// use aletheiadb::sql::SqlConverter;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let converter = SqlConverter::new();
/// let query = converter.convert_sql("SELECT * FROM nodes WHERE label = 'Person'")?;
/// # Ok(())
/// # }
/// ```
pub struct SqlConverter {
    /// Parameter bindings
    parameters: HashMap<String, SqlParameterValue>,
}

impl SqlConverter {
    /// Create a new SQL converter.
    pub fn new() -> Self {
        SqlConverter {
            parameters: HashMap::new(),
        }
    }

    /// Create a converter with pre-bound parameters.
    pub fn with_parameters(parameters: HashMap<String, SqlParameterValue>) -> Self {
        SqlConverter { parameters }
    }

    /// Bind a parameter value.
    pub fn bind(&mut self, name: impl Into<String>, value: SqlParameterValue) -> &mut Self {
        self.parameters.insert(name.into(), value);
        self
    }

    /// Convert a SQL string to a Query.
    pub fn convert_sql(&self, sql: &str) -> Result<Query, SqlError> {
        // Pipeline: SQL → temporal → match → vector → sqlparser → convert → wire up ops
        let extracted_temporal = temporal_parser::extract_temporal_clauses(sql)?;
        let extracted_match = match_parser::extract_match_clauses(&extracted_temporal.cleaned_sql)?;
        let extracted_vector = vector_parser::extract_vector_clauses(&extracted_match.cleaned_sql)?;

        // Parse the cleaned SQL (without temporal, MATCH, or vector clauses)
        let stmt = SqlParser::parse(&extracted_vector.cleaned_sql)?;

        // Convert to Query and add temporal context
        let mut query = self.convert(&stmt)?;
        query.temporal_context = extracted_temporal.to_temporal_context()?;

        // Insert graph traversal ops after source ops (ScanNodes/ScanEdges)
        // but before filter/projection/sort/limit ops.
        for pattern in extracted_match.patterns.iter().rev() {
            let insert_pos = query
                .ops
                .iter()
                .position(|op| !matches!(op, QueryOp::ScanNodes { .. } | QueryOp::ScanEdges { .. }))
                .unwrap_or(query.ops.len());
            query.ops.insert(insert_pos, pattern.to_query_op());
        }

        // Resolve and insert vector ops
        for vector_op in &extracted_vector.vector_ops {
            self.apply_vector_op(&mut query, vector_op)?;
        }

        Ok(query)
    }

    /// Convert a SQL statement to a Query.
    pub fn convert(&self, stmt: &Statement) -> Result<Query, SqlError> {
        match stmt {
            Statement::Query(query) => self.convert_query(query),
            _ => Err(SqlError::UnsupportedFeature(format!(
                "Only SELECT queries are supported, got: {:?}",
                stmt
            ))),
        }
    }

    /// Convert a SQL SELECT query to a Query.
    fn convert_query(&self, query: &SqlQuery) -> Result<Query, SqlError> {
        // Handle the body of the query
        let select = match query.body.as_ref() {
            SetExpr::Select(select) => select,
            _ => {
                return Err(SqlError::UnsupportedFeature(
                    "Only simple SELECT queries are supported".to_string(),
                ));
            }
        };

        let mut ops = Vec::new();

        // Convert FROM clause
        self.convert_from(&select.from, &mut ops)?;

        // Convert WHERE clause. Edge-property `WHERE` over a `FROM edges` scan is
        // evaluated for real (Issue #3622): the executor detects an
        // `EdgeScan`-rooted stream and runs the shared `FilterIterator` in
        // edge-property mode, matching each property leaf against the edge's own
        // properties. (SQL only ever sources edges via a bare `FROM edges`, a
        // pure edge stream, so every bare property leaf unambiguously refers to
        // the edge.)
        if let Some(ref selection) = select.selection {
            let predicate = self.convert_expr_to_predicate(selection)?;
            ops.push(QueryOp::Filter(predicate));
        }

        // Convert SELECT projection
        self.convert_projection(&select.projection, &mut ops)?;

        // Convert ORDER BY. A property-key `ORDER BY` over `FROM edges` is
        // likewise evaluated for real (Issue #3622): the `SortIterator` reads the
        // edge's own properties when its input is `EdgeScan`-rooted.
        for order_by in &query.order_by {
            self.convert_order_by(order_by, &mut ops)?;
        }

        // Convert OFFSET (must come before LIMIT for correct SQL semantics:
        // skip N rows first, then take M rows from the result)
        if let Some(ref offset) = query.offset {
            let n = self.expr_to_usize(&offset.value)?;
            ops.push(QueryOp::Skip(n));
        }

        // Convert LIMIT
        if let Some(ref limit) = query.limit {
            let n = self.expr_to_usize(limit)?;
            ops.push(QueryOp::Limit(n));
        }

        Ok(Query {
            ops,
            // Temporal context is set by convert_sql() after extraction
            temporal_context: None,
            hints: QueryHints::default(),
            // SQL namespace scoping is a follow-up (PR2b); no scope today.
            scope: None,
            // SQL-parsed queries carry no per-call resource-limit override
            // (Issue #3368 is a Rust `QueryBuilder`-only API in v1).
            limits: None,
        })
    }

    /// Convert FROM clause to source operations.
    fn convert_from(
        &self,
        from: &[TableWithJoins],
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), SqlError> {
        if from.is_empty() {
            return Err(SqlError::MissingClause(
                "FROM clause is required".to_string(),
            ));
        }

        if from.len() > 1 {
            return Err(SqlError::UnsupportedFeature(
                "Multiple tables (joins) not yet supported".to_string(),
            ));
        }

        let table = &from[0];
        if !table.joins.is_empty() {
            return Err(SqlError::UnsupportedFeature(
                "JOIN clauses are not yet supported. Use MATCH for graph traversal: \
                 MATCH (source)-[:EDGE_TYPE]->(target)"
                    .to_string(),
            ));
        }

        match &table.relation {
            TableFactor::Table { name, .. } => {
                let table_name = name.to_string().to_lowercase();
                match table_name.as_str() {
                    "nodes" => {
                        ops.push(QueryOp::ScanNodes { label: None });
                    }
                    "edges" => {
                        ops.push(QueryOp::ScanEdges { edge_type: None });
                    }
                    _ => {
                        // Treat other table names as label filters
                        // e.g., "SELECT * FROM Person" scans nodes with label 'Person'
                        ops.push(QueryOp::ScanNodes {
                            label: Some(table_name),
                        });
                    }
                }
            }
            _ => {
                return Err(SqlError::UnsupportedFeature(
                    "Complex table expressions not supported".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Convert SELECT projection.
    fn convert_projection(
        &self,
        projection: &[SelectItem],
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), SqlError> {
        let mut columns = Vec::new();
        let mut is_star = false;

        for item in projection {
            match item {
                SelectItem::Wildcard(_) => {
                    is_star = true;
                }
                SelectItem::UnnamedExpr(expr) => {
                    if let Some(col) = self.expr_to_column_name(expr) {
                        columns.push(col);
                    }
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    if self.expr_to_column_name(expr).is_some() {
                        columns.push(alias.value.clone());
                    }
                }
                SelectItem::QualifiedWildcard(_, _) => {
                    is_star = true;
                }
            }
        }

        // If not SELECT *, add projection
        if !is_star && !columns.is_empty() {
            ops.push(QueryOp::Project(columns));
        }

        Ok(())
    }

    /// Convert ORDER BY clause.
    ///
    /// A property-key `ORDER BY` over a `FROM edges` scan is honored: the
    /// executor runs the `SortIterator` in edge-property mode for an
    /// `EdgeScan`-rooted stream, so edge rows are sorted by their own properties
    /// (Issue #3622). `ORDER BY score`/`timestamp` are unaffected.
    fn convert_order_by(
        &self,
        order_by: &OrderByExpr,
        ops: &mut Vec<QueryOp>,
    ) -> Result<(), SqlError> {
        let key = match &order_by.expr {
            Expr::Identifier(ident) => {
                let name = ident.value.to_lowercase();
                match name.as_str() {
                    "score" => SortKey::Score,
                    "timestamp" => SortKey::Timestamp,
                    _ => SortKey::Property(ident.value.clone()),
                }
            }
            Expr::CompoundIdentifier(parts) => {
                // Handle table.column syntax
                let col = parts.last().map(|p| p.value.clone()).ok_or_else(|| {
                    SqlError::InvalidColumn("Empty compound identifier in ORDER BY".to_string())
                })?;
                SortKey::Property(col)
            }
            _ => {
                return Err(SqlError::UnsupportedFeature(
                    "Complex ORDER BY expressions not yet supported. Use simple column names (e.g., ORDER BY name DESC)".to_string(),
                ));
            }
        };

        let descending = order_by.asc.map(|asc| !asc).unwrap_or(false);

        ops.push(QueryOp::Sort { key, descending });

        Ok(())
    }

    /// Convert a SQL expression to a predicate.
    fn convert_expr_to_predicate(&self, expr: &Expr) -> Result<Predicate, SqlError> {
        match expr {
            Expr::BinaryOp { left, op, right } => self.convert_binary_op(left, op, right),
            Expr::Nested(inner) => self.convert_expr_to_predicate(inner),
            Expr::IsNull(inner) => {
                let key = self.expr_to_property_key(inner)?;
                Ok(Predicate::Eq {
                    key,
                    value: PredicateValue::Null,
                })
            }
            Expr::IsNotNull(inner) => {
                let key = self.expr_to_property_key(inner)?;
                Ok(Predicate::Ne {
                    key,
                    value: PredicateValue::Null,
                })
            }
            Expr::InList {
                expr,
                list,
                negated,
            } => {
                let key = self.expr_to_property_key(expr)?;
                let values: Result<Vec<PredicateValue>, SqlError> =
                    list.iter().map(|e| self.expr_to_value(e)).collect();
                let pred = Predicate::In {
                    key,
                    values: values?,
                };
                if *negated { Ok(!pred) } else { Ok(pred) }
            }
            Expr::Like {
                expr,
                pattern,
                negated,
                ..
            } => {
                let key = self.expr_to_property_key(expr)?;
                let pattern_str = self.expr_to_string(pattern)?;

                // Convert LIKE pattern to appropriate predicate using slicing
                // to correctly handle patterns like 'a%b'
                let pred = if pattern_str.starts_with('%')
                    && pattern_str.ends_with('%')
                    && pattern_str.len() > 1
                {
                    // %substring% -> Contains
                    let substring = pattern_str[1..pattern_str.len() - 1].to_string();
                    Predicate::Contains { key, substring }
                } else if pattern_str.ends_with('%') && !pattern_str.starts_with('%') {
                    // prefix% -> StartsWith
                    let prefix = pattern_str[..pattern_str.len() - 1].to_string();
                    Predicate::StartsWith { key, prefix }
                } else if pattern_str.starts_with('%') && !pattern_str.ends_with('%') {
                    // %suffix -> EndsWith
                    let suffix = pattern_str[1..].to_string();
                    Predicate::EndsWith { key, suffix }
                } else {
                    // Exact match (no wildcards or complex pattern)
                    Predicate::Eq {
                        key,
                        value: PredicateValue::String(pattern_str),
                    }
                };

                if *negated { Ok(!pred) } else { Ok(pred) }
            }
            _ => Err(SqlError::UnsupportedFeature(format!(
                "Expression type not supported in WHERE: {:?}",
                expr
            ))),
        }
    }

    /// Convert a binary operation to a predicate.
    fn convert_binary_op(
        &self,
        left: &Expr,
        op: &BinaryOperator,
        right: &Expr,
    ) -> Result<Predicate, SqlError> {
        // Handle logical operators
        match op {
            BinaryOperator::And => {
                let l = self.convert_expr_to_predicate(left)?;
                let r = self.convert_expr_to_predicate(right)?;
                return Ok(l.and(r));
            }
            BinaryOperator::Or => {
                let l = self.convert_expr_to_predicate(left)?;
                let r = self.convert_expr_to_predicate(right)?;
                return Ok(l.or(r));
            }
            _ => {}
        }

        // Handle comparison operators
        let key = self.expr_to_property_key(left)?;
        let value = self.expr_to_value(right)?;

        match op {
            BinaryOperator::Eq => Ok(Predicate::Eq { key, value }),
            BinaryOperator::NotEq => Ok(Predicate::Ne { key, value }),
            BinaryOperator::Lt => Ok(Predicate::Lt { key, value }),
            BinaryOperator::LtEq => Ok(Predicate::Lte { key, value }),
            BinaryOperator::Gt => Ok(Predicate::Gt { key, value }),
            BinaryOperator::GtEq => Ok(Predicate::Gte { key, value }),
            _ => Err(SqlError::UnsupportedFeature(format!(
                "Operator not supported: {:?}",
                op
            ))),
        }
    }

    /// Extract property key from expression.
    fn expr_to_property_key(&self, expr: &Expr) -> Result<String, SqlError> {
        match expr {
            Expr::Identifier(ident) => Ok(ident.value.clone()),
            Expr::CompoundIdentifier(parts) => {
                // Handle table.column syntax - return just the column name
                parts
                    .last()
                    .map(|p| p.value.clone())
                    .ok_or_else(|| SqlError::InvalidColumn("Empty compound identifier".to_string()))
            }
            _ => Err(SqlError::InvalidColumn(format!(
                "Cannot extract property key from: {:?}",
                expr
            ))),
        }
    }

    /// Convert expression to predicate value.
    fn expr_to_value(&self, expr: &Expr) -> Result<PredicateValue, SqlError> {
        match expr {
            Expr::Value(value) => self.value_to_predicate_value(value),
            Expr::Identifier(ident) => {
                // Check if it's a parameter reference
                if let Some(param) = self.parameters.get(&ident.value) {
                    match param {
                        SqlParameterValue::Scalar(v) => Ok(v.clone()),
                        _ => Err(SqlError::TypeError(
                            "Expected scalar parameter value".to_string(),
                        )),
                    }
                } else {
                    Err(SqlError::ParameterError(format!(
                        "Unknown parameter: {}",
                        ident.value
                    )))
                }
            }
            Expr::UnaryOp { op, expr } => {
                // Handle negative numbers
                match op {
                    sqlparser::ast::UnaryOperator::Minus => {
                        let inner = self.expr_to_value(expr)?;
                        match inner {
                            PredicateValue::Int(n) => Ok(PredicateValue::Int(-n)),
                            PredicateValue::Float(n) => Ok(PredicateValue::Float(-n)),
                            _ => Err(SqlError::TypeError(
                                "Cannot negate non-numeric value".to_string(),
                            )),
                        }
                    }
                    _ => Err(SqlError::UnsupportedFeature(format!(
                        "Unary operator not supported: {:?}",
                        op
                    ))),
                }
            }
            _ => Err(SqlError::UnsupportedFeature(format!(
                "Expression type not supported as value: {:?}",
                expr
            ))),
        }
    }

    /// Convert SQL value to predicate value.
    fn value_to_predicate_value(&self, value: &Value) -> Result<PredicateValue, SqlError> {
        match value {
            Value::Null => Ok(PredicateValue::Null),
            Value::Boolean(b) => Ok(PredicateValue::Bool(*b)),
            Value::Number(n, _) => {
                // Try parsing as i64 first, then f64
                if let Ok(i) = n.parse::<i64>() {
                    Ok(PredicateValue::Int(i))
                } else if let Ok(f) = n.parse::<f64>() {
                    Ok(PredicateValue::Float(f))
                } else {
                    Err(SqlError::TypeError(format!("Invalid number: {}", n)))
                }
            }
            Value::SingleQuotedString(s) | Value::DoubleQuotedString(s) => {
                Ok(PredicateValue::String(s.clone()))
            }
            _ => Err(SqlError::UnsupportedFeature(format!(
                "Value type not supported: {:?}",
                value
            ))),
        }
    }

    /// Extract column name from expression.
    fn expr_to_column_name(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Identifier(ident) => Some(ident.value.clone()),
            Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.clone()),
            _ => None,
        }
    }

    /// Convert expression to usize.
    fn expr_to_usize(&self, expr: &Expr) -> Result<usize, SqlError> {
        match expr {
            Expr::Value(Value::Number(n, _)) => n
                .parse::<usize>()
                .map_err(|_| SqlError::TypeError(format!("Expected positive integer, got: {}", n))),
            _ => Err(SqlError::TypeError(format!(
                "Expected integer literal, got: {:?}",
                expr
            ))),
        }
    }

    /// Convert expression to string.
    fn expr_to_string(&self, expr: &Expr) -> Result<String, SqlError> {
        match expr {
            Expr::Value(Value::SingleQuotedString(s) | Value::DoubleQuotedString(s)) => {
                Ok(s.clone())
            }
            _ => Err(SqlError::TypeError(format!(
                "Expected string literal, got: {:?}",
                expr
            ))),
        }
    }

    // =========================================================================
    // Vector operation wiring
    // =========================================================================

    /// Resolve an `EmbeddingRef` to a concrete `Arc<[f32]>` using bound parameters.
    fn resolve_embedding(&self, r: &EmbeddingRef) -> Result<Arc<[f32]>, SqlError> {
        match r {
            EmbeddingRef::Literal(v) => Ok(v.clone().into()),
            EmbeddingRef::Parameter(name) => {
                // Strip leading $ if present
                let clean_name = name.strip_prefix('$').unwrap_or(name);
                match self.parameters.get(clean_name) {
                    Some(SqlParameterValue::Embedding(e)) => Ok(e.clone()),
                    Some(_) => Err(SqlError::TypeError(format!(
                        "Parameter '{}' is not an embedding",
                        clean_name
                    ))),
                    None => Err(SqlError::ParameterError(format!(
                        "Unbound embedding parameter: {}",
                        clean_name
                    ))),
                }
            }
        }
    }

    /// Apply a parsed vector operation to the query ops pipeline.
    fn apply_vector_op(&self, query: &mut Query, op: &VectorOp) -> Result<(), SqlError> {
        match op {
            VectorOp::KnnOrderBy {
                property_key,
                embedding_ref,
                metric,
            } => {
                let embedding = self.resolve_embedding(embedding_ref)?;

                // Detect whether we have graph traversal ops in the pipeline.
                // If so, this is a hybrid query: rank traversal results by similarity.
                let has_traversal = query.ops.iter().any(|op| {
                    matches!(
                        op,
                        QueryOp::TraverseOut { .. }
                            | QueryOp::TraverseIn { .. }
                            | QueryOp::TraverseBoth { .. }
                    )
                });

                let k = query.ops.iter().find_map(|op| {
                    if let QueryOp::Limit(n) = op {
                        Some(*n)
                    } else {
                        None
                    }
                });

                if has_traversal {
                    // Hybrid mode: rank traversal results by similarity.
                    // Insert RankBySimilarity before the Limit op.
                    let pos = query
                        .ops
                        .iter()
                        .position(|op| matches!(op, QueryOp::Limit(_)))
                        .unwrap_or(query.ops.len());
                    query.ops.insert(
                        pos,
                        QueryOp::RankBySimilarity {
                            embedding,
                            top_k: k,
                            property_key: Some(property_key.clone()),
                            metric: crate::core::vector::DistanceMetric::Cosine,
                            threshold: None,
                            score_alias: None,
                        },
                    );
                } else {
                    // Standalone k-NN search.
                    let limit = k.unwrap_or(10);

                    // If OFFSET is present, VectorSearch must fetch enough rows so
                    // that after Skip(offset) we still have `limit` results.
                    let offset = query.ops.iter().find_map(|op| {
                        if let QueryOp::Skip(n) = op {
                            Some(*n)
                        } else {
                            None
                        }
                    });
                    let effective_k = limit + offset.unwrap_or(0);

                    // Remove the Limit op since VectorSearch has its own k.
                    // Keep Skip so offset semantics are preserved.
                    query.ops.retain(|op| !matches!(op, QueryOp::Limit(_)));

                    // Insert VectorSearch after source ops.
                    let pos = query
                        .ops
                        .iter()
                        .position(|op| {
                            !matches!(op, QueryOp::ScanNodes { .. } | QueryOp::ScanEdges { .. })
                        })
                        .unwrap_or(query.ops.len());
                    query.ops.insert(
                        pos,
                        QueryOp::VectorSearch {
                            embedding,
                            k: effective_k,
                            metric: *metric,
                            property_key: Some(property_key.clone()),
                        },
                    );

                    // Re-add Limit after Skip so the final row count equals `limit`.
                    if offset.is_some() {
                        query.ops.push(QueryOp::Limit(limit));
                    }
                }
            }
            VectorOp::KnnFunction {
                property_key,
                embedding_ref,
                k,
                ..
            } => {
                let embedding = self.resolve_embedding(embedding_ref)?;

                // Preserve label filter from the ScanNodes we are about to remove.
                // e.g. `FROM KNN('Documents', ...)` produces ScanNodes { label: Some("documents") }.
                let label = query.ops.iter().find_map(|op| {
                    if let QueryOp::ScanNodes { label: Some(l) } = op {
                        Some(l.clone())
                    } else {
                        None
                    }
                });

                // Replace the scan with VectorSearch.
                query
                    .ops
                    .retain(|op| !matches!(op, QueryOp::ScanNodes { .. }));
                query.ops.insert(
                    0,
                    QueryOp::VectorSearch {
                        embedding,
                        k: *k,
                        metric: DistanceMetric::Cosine,
                        property_key: Some(property_key.clone()),
                    },
                );

                // Re-apply label filter so results are scoped to the original label.
                if let Some(l) = label {
                    query.ops.insert(1, QueryOp::FilterLabel(l));
                }
            }
            VectorOp::SimilarToFilter {
                property_key,
                embedding_ref,
                threshold,
            } => {
                // A full implementation would add a dedicated VectorFilter QueryOp.
                // For now we over-fetch with VectorSearch and post-filter by threshold.
                let embedding = self.resolve_embedding(embedding_ref)?;

                let k = SIMILAR_TO_DEFAULT_K;
                let pos = query
                    .ops
                    .iter()
                    .position(|op| {
                        !matches!(op, QueryOp::ScanNodes { .. } | QueryOp::ScanEdges { .. })
                    })
                    .unwrap_or(query.ops.len());
                query.ops.insert(
                    pos,
                    QueryOp::VectorSearch {
                        embedding,
                        k,
                        metric: DistanceMetric::Cosine,
                        property_key: Some(property_key.clone()),
                    },
                );

                // Enforce the similarity threshold as a post-search filter.
                // VectorSearch returns a "score" property for each result;
                // we keep only results whose score >= the user-specified threshold.
                query.ops.insert(
                    pos + 1,
                    QueryOp::Filter(Predicate::Gte {
                        key: "score".to_string(),
                        value: PredicateValue::Float(*threshold),
                    }),
                );
            }
        }

        Ok(())
    }
}

impl Default for SqlConverter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a SQL query and convert it to a AletheiaDB Query.
///
/// This is the simplest way to execute a SQL query.
///
/// # Example
///
/// ```rust
/// use aletheiadb::sql::parse_sql;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let query = parse_sql("SELECT * FROM nodes WHERE label = 'Person' LIMIT 10")?;
/// # Ok(())
/// # }
/// ```
pub fn parse_sql(sql: &str) -> Result<Query, SqlError> {
    let converter = SqlConverter::new();
    converter.convert_sql(sql)
}

/// Parse a SQL query with parameters.
///
/// # Example
///
/// ```rust
/// use aletheiadb::sql::{parse_sql_with_params, SqlParameterValue};
/// use aletheiadb::query::ir::PredicateValue;
/// use std::collections::HashMap;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut params = HashMap::new();
/// params.insert("min_age".to_string(), SqlParameterValue::Scalar(PredicateValue::Int(21)));
///
/// let query = parse_sql_with_params(
///     "SELECT * FROM nodes WHERE age > min_age",
///     params
/// )?;
/// # Ok(())
/// # }
/// ```
pub fn parse_sql_with_params(
    sql: &str,
    params: HashMap<String, SqlParameterValue>,
) -> Result<Query, SqlError> {
    let converter = SqlConverter::with_parameters(params);
    converter.convert_sql(sql)
}
