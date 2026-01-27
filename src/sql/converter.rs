//! SQL to QueryOp Converter.
//!
//! This module converts parsed SQL AST from sqlparser-rs into GallifreyDB's
//! internal Query representation (QueryOp operations).

use std::collections::HashMap;
use std::sync::Arc;

use sqlparser::ast::{
    BinaryOperator, Expr, OrderByExpr, Query as SqlQuery, SelectItem, SetExpr, Statement,
    TableFactor, TableWithJoins, Value,
};

use crate::query::builder::Query;
use crate::query::ir::{Predicate, PredicateValue, QueryOp, SortKey};
use crate::query::plan::QueryHints;

use super::error::SqlError;
use super::parser::SqlParser;
use super::temporal_parser;

/// Parameter values that can be bound to SQL queries.
#[derive(Debug, Clone)]
pub enum SqlParameterValue {
    /// Scalar value (string, int, float, bool)
    Scalar(PredicateValue),
    /// Vector embedding for k-NN search
    Embedding(Arc<[f32]>),
}

/// Converter from SQL AST to GallifreyDB Query.
///
/// # Example
///
/// ```rust,ignore
/// use gallifreydb::sql::SqlConverter;
///
/// let converter = SqlConverter::new();
/// let query = converter.convert_sql("SELECT * FROM nodes WHERE label = 'Person'")?;
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
        // Extract temporal clauses first
        let extracted = temporal_parser::extract_temporal_clauses(sql)?;

        // Parse the cleaned SQL (without temporal clauses)
        let stmt = SqlParser::parse(&extracted.cleaned_sql)?;

        // Convert to Query and add temporal context
        let mut query = self.convert(&stmt)?;
        query.temporal_context = extracted.to_temporal_context()?;

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
        // Temporal context is populated by convert_sql() via temporal preprocessing
        let temporal_context = None;

        // Convert FROM clause
        self.convert_from(&select.from, &mut ops)?;

        // Convert WHERE clause
        if let Some(ref selection) = select.selection {
            let predicate = self.convert_expr_to_predicate(selection)?;
            ops.push(QueryOp::Filter(predicate));
        }

        // Convert SELECT projection
        self.convert_projection(&select.projection, &mut ops)?;

        // Convert ORDER BY
        for order_by in &query.order_by {
            self.convert_order_by(order_by, &mut ops)?;
        }

        // Convert LIMIT
        if let Some(ref limit) = query.limit {
            let n = self.expr_to_usize(limit)?;
            ops.push(QueryOp::Limit(n));
        }

        // Convert OFFSET
        if let Some(ref offset) = query.offset {
            let n = self.expr_to_usize(&offset.value)?;
            ops.push(QueryOp::Skip(n));
        }

        Ok(Query {
            ops,
            temporal_context,
            hints: QueryHints::default(),
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
                "JOIN clauses not yet supported".to_string(),
            ));
        }

        match &table.relation {
            TableFactor::Table { name, alias: _, .. } => {
                let table_name = name.to_string().to_lowercase();
                match table_name.as_str() {
                    "nodes" => {
                        ops.push(QueryOp::ScanNodes { label: None });
                    }
                    "edges" => {
                        // For edges, we'll need to implement edge scanning
                        // For now, return unsupported
                        return Err(SqlError::UnsupportedFeature(
                            "Edge scanning not yet implemented".to_string(),
                        ));
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
                    "Complex ORDER BY expressions not supported".to_string(),
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
}

impl Default for SqlConverter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a SQL query and convert it to a GallifreyDB Query.
///
/// This is the simplest way to execute a SQL query.
///
/// # Example
///
/// ```rust,ignore
/// use gallifreydb::sql::parse_sql;
///
/// let query = parse_sql("SELECT * FROM nodes WHERE label = 'Person' LIMIT 10")?;
/// ```
pub fn parse_sql(sql: &str) -> Result<Query, SqlError> {
    let converter = SqlConverter::new();
    converter.convert_sql(sql)
}

/// Parse a SQL query with parameters.
///
/// # Example
///
/// ```rust,ignore
/// use gallifreydb::sql::{parse_sql_with_params, SqlParameterValue};
/// use gallifreydb::query::ir::PredicateValue;
///
/// let mut params = HashMap::new();
/// params.insert("min_age".to_string(), SqlParameterValue::Scalar(PredicateValue::Int(21)));
///
/// let query = parse_sql_with_params(
///     "SELECT * FROM nodes WHERE age > min_age",
///     params
/// )?;
/// ```
pub fn parse_sql_with_params(
    sql: &str,
    params: HashMap<String, SqlParameterValue>,
) -> Result<Query, SqlError> {
    let converter = SqlConverter::with_parameters(params);
    converter.convert_sql(sql)
}
