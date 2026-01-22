//! Abstract Syntax Tree for GQL (Gallifrey Query Language)
//!
//! This module defines the AST types that represent parsed GQL queries.
//! The AST is produced by the parser and consumed by the query planner
//! to generate a logical query plan.

use std::sync::Arc;

use crate::index::vector::DistanceMetric;

/// A complete GQL query.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryAst {
    /// Optional temporal clause (AS OF or BETWEEN)
    pub temporal: Option<TemporalClause>,
    /// Main query source (MATCH or vector search)
    pub source: SourceClause,
    /// Optional ranking clause (RANK BY SIMILARITY)
    pub rank: Option<RankClause>,
    /// WHERE predicates
    pub where_clause: Option<WhereClause>,
    /// RETURN clause
    pub return_clause: Option<ReturnClause>,
    /// ORDER BY clause
    pub order: Option<OrderClause>,
    /// SKIP clause
    pub skip: Option<usize>,
    /// LIMIT clause
    pub limit: Option<usize>,
}

impl QueryAst {
    /// Create a new query AST with the given source clause.
    pub fn new(source: SourceClause) -> Self {
        QueryAst {
            temporal: None,
            source,
            rank: None,
            where_clause: None,
            return_clause: None,
            order: None,
            skip: None,
            limit: None,
        }
    }

    /// Add a temporal clause to the query.
    #[must_use]
    pub fn with_temporal(mut self, temporal: TemporalClause) -> Self {
        self.temporal = Some(temporal);
        self
    }

    /// Add a rank clause to the query.
    #[must_use]
    pub fn with_rank(mut self, rank: RankClause) -> Self {
        self.rank = Some(rank);
        self
    }

    /// Add a WHERE clause to the query.
    #[must_use]
    pub fn with_where(mut self, where_clause: WhereClause) -> Self {
        self.where_clause = Some(where_clause);
        self
    }

    /// Add a RETURN clause to the query.
    #[must_use]
    pub fn with_return(mut self, return_clause: ReturnClause) -> Self {
        self.return_clause = Some(return_clause);
        self
    }

    /// Add an ORDER BY clause to the query.
    #[must_use]
    pub fn with_order(mut self, order: OrderClause) -> Self {
        self.order = Some(order);
        self
    }

    /// Add a SKIP clause to the query.
    #[must_use]
    pub fn with_skip(mut self, skip: usize) -> Self {
        self.skip = Some(skip);
        self
    }

    /// Add a LIMIT clause to the query.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Check if this query has temporal context.
    pub fn is_temporal(&self) -> bool {
        self.temporal.is_some()
    }

    /// Check if this query involves vector operations.
    pub fn has_vector_ops(&self) -> bool {
        matches!(
            self.source,
            SourceClause::VectorSearch { .. } | SourceClause::FindSimilar { .. }
        ) || self.rank.is_some()
    }
}

/// Temporal clause for time-travel queries.
#[derive(Debug, Clone, PartialEq)]
pub enum TemporalClause {
    /// AS OF timestamp [, transaction_time]
    AsOf {
        /// Valid time
        valid_time: TimestampLiteral,
        /// Optional transaction time
        transaction_time: Option<TimestampLiteral>,
    },
    /// BETWEEN start AND end
    Between {
        /// Start time
        start: TimestampLiteral,
        /// End time
        end: TimestampLiteral,
    },
}

/// A timestamp literal (string or integer).
#[derive(Debug, Clone, PartialEq)]
pub enum TimestampLiteral {
    /// ISO 8601 string: '2024-01-15T10:00:00Z'
    String(String),
    /// Unix timestamp in milliseconds
    Integer(i64),
}

/// Main source clause of a query.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceClause {
    /// MATCH pattern
    Match(Vec<Pattern>),
    /// SIMILAR TO embedding
    VectorSearch {
        /// The embedding to search for (parameter or literal)
        embedding: EmbeddingRef,
        /// Distance metric (optional, defaults to Cosine)
        metric: Option<DistanceMetric>,
        /// Result limit
        limit: usize,
    },
    /// FIND SIMILAR TO (node_ref)
    FindSimilar {
        /// Reference to the source node
        node_ref: NodeRef,
        /// Result limit
        limit: usize,
    },
}

/// An embedding reference (parameter or literal array).
#[derive(Debug, Clone, PartialEq)]
pub enum EmbeddingRef {
    /// Parameter reference: $embedding
    Parameter(String),
    /// Literal array: [0.1, 0.2, 0.3, ...]
    Literal(Arc<[f32]>),
}

/// A reference to a node.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeRef {
    /// Reference by identifier: (n)
    Identifier(String),
    /// Reference by ID: (123)
    Id(u64),
    /// Reference by parameter: ($node_id)
    Parameter(String),
}

/// A graph pattern in a MATCH clause.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    /// Sequence of pattern elements (nodes and relationships)
    pub elements: Vec<PatternElement>,
}

impl Pattern {
    /// Create a new pattern with a single node.
    pub fn node(node: NodePattern) -> Self {
        Pattern {
            elements: vec![PatternElement::Node(node)],
        }
    }

    /// Add a relationship and node to the pattern.
    #[must_use]
    pub fn then(mut self, rel: RelationshipPattern, node: NodePattern) -> Self {
        self.elements.push(PatternElement::Relationship(rel));
        self.elements.push(PatternElement::Node(node));
        self
    }
}

/// An element in a pattern (either a node or relationship).
#[derive(Debug, Clone, PartialEq)]
pub enum PatternElement {
    /// A node pattern: (n:Label {props})
    Node(NodePattern),
    /// A relationship pattern: -[:REL]->
    Relationship(RelationshipPattern),
}

/// A node pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    /// Optional variable binding
    pub variable: Option<String>,
    /// Optional label
    pub label: Option<String>,
    /// Optional inline properties
    pub properties: Option<PropertyMap>,
}

impl NodePattern {
    /// Create an empty node pattern: ()
    pub fn empty() -> Self {
        NodePattern {
            variable: None,
            label: None,
            properties: None,
        }
    }

    /// Create a node pattern with just a variable: (n)
    pub fn var(name: impl Into<String>) -> Self {
        NodePattern {
            variable: Some(name.into()),
            label: None,
            properties: None,
        }
    }

    /// Create a node pattern with variable and label: (n:Person)
    pub fn with_label(name: impl Into<String>, label: impl Into<String>) -> Self {
        NodePattern {
            variable: Some(name.into()),
            label: Some(label.into()),
            properties: None,
        }
    }

    /// Add properties to the pattern.
    #[must_use]
    pub fn with_properties(mut self, properties: PropertyMap) -> Self {
        self.properties = Some(properties);
        self
    }
}

/// A relationship pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationshipPattern {
    /// Optional variable binding
    pub variable: Option<String>,
    /// Optional relationship type (label)
    pub rel_type: Option<String>,
    /// Direction of the relationship
    pub direction: RelationshipDirection,
    /// Depth specification for variable-length paths
    pub depth: Option<DepthSpec>,
}

impl RelationshipPattern {
    /// Create an outgoing relationship: -[]->(
    pub fn outgoing() -> Self {
        RelationshipPattern {
            variable: None,
            rel_type: None,
            direction: RelationshipDirection::Outgoing,
            depth: None,
        }
    }

    /// Create an incoming relationship: <-[]-
    pub fn incoming() -> Self {
        RelationshipPattern {
            variable: None,
            rel_type: None,
            direction: RelationshipDirection::Incoming,
            depth: None,
        }
    }

    /// Create a bidirectional relationship: -[]-
    pub fn both() -> Self {
        RelationshipPattern {
            variable: None,
            rel_type: None,
            direction: RelationshipDirection::Both,
            depth: None,
        }
    }

    /// Add a relationship type: -[:KNOWS]->
    #[must_use]
    pub fn with_type(mut self, rel_type: impl Into<String>) -> Self {
        self.rel_type = Some(rel_type.into());
        self
    }

    /// Add a variable binding: -[r:KNOWS]->
    #[must_use]
    pub fn with_variable(mut self, var: impl Into<String>) -> Self {
        self.variable = Some(var.into());
        self
    }

    /// Add a depth specification: -[:KNOWS*1..3]->
    #[must_use]
    pub fn with_depth(mut self, depth: DepthSpec) -> Self {
        self.depth = Some(depth);
        self
    }
}

/// Direction of a relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationshipDirection {
    /// Outgoing: ->
    Outgoing,
    /// Incoming: <-
    Incoming,
    /// Both: - (undirected)
    Both,
}

/// Depth specification for variable-length paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum DepthSpec {
    /// Exactly N hops: *N
    Exact(usize),
    /// Up to N hops: *..N
    Max(usize),
    /// Range of hops: *M..N
    Range { min: usize, max: usize },
    /// Unbounded: *
    Variable,
}

impl DepthSpec {
    /// Create a single-hop depth.
    pub fn one() -> Self {
        DepthSpec::Exact(1)
    }

    /// Create an exact depth.
    pub fn exact(n: usize) -> Self {
        DepthSpec::Exact(n)
    }

    /// Create a range depth.
    pub fn range(min: usize, max: usize) -> Self {
        DepthSpec::Range { min, max }
    }
}

/// A map of properties for inline property matching.
pub type PropertyMap = Vec<(String, PropertyValue)>;

/// A property value in a pattern or predicate.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// Null value
    Null,
    /// Boolean value
    Bool(bool),
    /// Integer value
    Int(i64),
    /// Float value
    Float(f64),
    /// String value
    String(String),
    /// Parameter reference
    Parameter(String),
}

impl From<bool> for PropertyValue {
    fn from(v: bool) -> Self {
        PropertyValue::Bool(v)
    }
}

impl From<i64> for PropertyValue {
    fn from(v: i64) -> Self {
        PropertyValue::Int(v)
    }
}

impl From<f64> for PropertyValue {
    fn from(v: f64) -> Self {
        PropertyValue::Float(v)
    }
}

impl From<String> for PropertyValue {
    fn from(v: String) -> Self {
        PropertyValue::String(v)
    }
}

impl From<&str> for PropertyValue {
    fn from(v: &str) -> Self {
        PropertyValue::String(v.to_string())
    }
}

/// RANK BY SIMILARITY clause.
#[derive(Debug, Clone, PartialEq)]
pub struct RankClause {
    /// The embedding to rank by
    pub embedding: EmbeddingRef,
    /// Optional TOP k limit
    pub top_k: Option<usize>,
}

/// WHERE clause containing a predicate.
#[derive(Debug, Clone, PartialEq)]
pub struct WhereClause {
    /// The predicate expression
    pub predicate: PredicateExpr,
}

/// A predicate expression.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum PredicateExpr {
    /// Comparison: n.prop = value
    Comparison {
        left: Expression,
        op: ComparisonOp,
        right: Expression,
    },
    /// Existence check: EXISTS(n.prop)
    Exists(PropertyAccess),
    /// NULL check: n.prop IS NULL
    IsNull(PropertyAccess),
    /// NOT NULL check: n.prop IS NOT NULL
    IsNotNull(PropertyAccess),
    /// String contains: n.prop CONTAINS 'str'
    Contains {
        property: PropertyAccess,
        substring: String,
    },
    /// String starts with: n.prop STARTS WITH 'str'
    StartsWith {
        property: PropertyAccess,
        prefix: String,
    },
    /// String ends with: n.prop ENDS WITH 'str'
    EndsWith {
        property: PropertyAccess,
        suffix: String,
    },
    /// IN list: n.prop IN [1, 2, 3]
    In {
        property: PropertyAccess,
        values: Vec<PropertyValue>,
    },
    /// Logical AND
    And(Box<PredicateExpr>, Box<PredicateExpr>),
    /// Logical OR
    Or(Box<PredicateExpr>, Box<PredicateExpr>),
    /// Logical NOT
    Not(Box<PredicateExpr>),
    /// Parenthesized expression
    Grouped(Box<PredicateExpr>),
}

impl PredicateExpr {
    /// Create an equality comparison.
    pub fn eq(left: Expression, right: Expression) -> Self {
        PredicateExpr::Comparison {
            left,
            op: ComparisonOp::Eq,
            right,
        }
    }

    /// Create a greater-than comparison.
    pub fn gt(left: Expression, right: Expression) -> Self {
        PredicateExpr::Comparison {
            left,
            op: ComparisonOp::Gt,
            right,
        }
    }

    /// Combine with AND.
    pub fn and(self, other: PredicateExpr) -> Self {
        PredicateExpr::And(Box::new(self), Box::new(other))
    }

    /// Combine with OR.
    pub fn or(self, other: PredicateExpr) -> Self {
        PredicateExpr::Or(Box::new(self), Box::new(other))
    }

    /// Negate this predicate.
    pub fn negate(self) -> Self {
        PredicateExpr::Not(Box::new(self))
    }
}

impl std::ops::Not for PredicateExpr {
    type Output = Self;

    fn not(self) -> Self::Output {
        PredicateExpr::Not(Box::new(self))
    }
}

/// Comparison operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    /// =
    Eq,
    /// <> or !=
    Ne,
    /// <
    Lt,
    /// <=
    Le,
    /// >
    Gt,
    /// >=
    Ge,
}

/// An expression (used in comparisons and projections).
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Expression {
    /// Property access: n.prop
    Property(PropertyAccess),
    /// Literal value
    Literal(PropertyValue),
    /// Parameter: $param
    Parameter(String),
    /// Function call: func(args)
    FunctionCall { name: String, args: Vec<Expression> },
}

impl Expression {
    /// Create a property access expression.
    pub fn property(var: impl Into<String>, prop: impl Into<String>) -> Self {
        Expression::Property(PropertyAccess {
            variable: var.into(),
            property: prop.into(),
        })
    }

    /// Create a literal expression.
    pub fn literal(value: PropertyValue) -> Self {
        Expression::Literal(value)
    }

    /// Create an integer literal expression.
    pub fn int(value: i64) -> Self {
        Expression::Literal(PropertyValue::Int(value))
    }

    /// Create a string literal expression.
    pub fn string(value: impl Into<String>) -> Self {
        Expression::Literal(PropertyValue::String(value.into()))
    }

    /// Create a parameter expression.
    pub fn param(name: impl Into<String>) -> Self {
        Expression::Parameter(name.into())
    }
}

/// Property access: variable.property
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyAccess {
    /// Variable name
    pub variable: String,
    /// Property name
    pub property: String,
}

impl PropertyAccess {
    /// Create a new property access.
    pub fn new(variable: impl Into<String>, property: impl Into<String>) -> Self {
        PropertyAccess {
            variable: variable.into(),
            property: property.into(),
        }
    }
}

/// RETURN clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnClause {
    /// Return items
    pub items: Vec<ReturnItem>,
    /// DISTINCT modifier
    pub distinct: bool,
}

impl ReturnClause {
    /// Create a new RETURN clause.
    pub fn new(items: Vec<ReturnItem>) -> Self {
        ReturnClause {
            items,
            distinct: false,
        }
    }

    /// Add DISTINCT modifier.
    #[must_use]
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }
}

/// An item in a RETURN clause.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    /// The expression to return
    pub expression: Expression,
    /// Optional alias: AS alias
    pub alias: Option<String>,
}

impl ReturnItem {
    /// Create a new return item.
    pub fn new(expression: Expression) -> Self {
        ReturnItem {
            expression,
            alias: None,
        }
    }

    /// Add an alias.
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = Some(alias.into());
        self
    }
}

/// COUNT(*) or COUNT(expr) return.
#[derive(Debug, Clone, PartialEq)]
pub enum CountExpr {
    /// COUNT(*)
    All,
    /// COUNT(expr)
    Expression(Expression),
}

/// ORDER BY clause.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderClause {
    /// Order items
    pub items: Vec<OrderItem>,
}

impl OrderClause {
    /// Create a new ORDER BY clause.
    pub fn new(items: Vec<OrderItem>) -> Self {
        OrderClause { items }
    }
}

/// An item in an ORDER BY clause.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderItem {
    /// Expression to order by
    pub expression: Expression,
    /// Sort direction (true = descending)
    pub descending: bool,
}

impl OrderItem {
    /// Create an ascending order item.
    pub fn asc(expression: Expression) -> Self {
        OrderItem {
            expression,
            descending: false,
        }
    }

    /// Create a descending order item.
    pub fn desc(expression: Expression) -> Self {
        OrderItem {
            expression,
            descending: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================
    // QueryAst Tests
    // =====================================================

    #[test]
    fn test_query_ast_new() {
        let source = SourceClause::Match(vec![Pattern::node(NodePattern::var("n"))]);
        let query = QueryAst::new(source);

        assert!(query.temporal.is_none());
        assert!(query.rank.is_none());
        assert!(query.where_clause.is_none());
        assert!(query.return_clause.is_none());
        assert!(query.order.is_none());
        assert!(query.skip.is_none());
        assert!(query.limit.is_none());
    }

    #[test]
    fn test_query_ast_with_temporal() {
        let source = SourceClause::Match(vec![Pattern::node(NodePattern::var("n"))]);
        let query = QueryAst::new(source).with_temporal(TemporalClause::AsOf {
            valid_time: TimestampLiteral::String("2024-01-15".to_string()),
            transaction_time: None,
        });

        assert!(query.is_temporal());
    }

    #[test]
    fn test_query_ast_has_vector_ops() {
        // Vector search
        let query = QueryAst::new(SourceClause::VectorSearch {
            embedding: EmbeddingRef::Parameter("emb".to_string()),
            metric: None,
            limit: 10,
        });
        assert!(query.has_vector_ops());

        // Find similar
        let query = QueryAst::new(SourceClause::FindSimilar {
            node_ref: NodeRef::Identifier("n".to_string()),
            limit: 10,
        });
        assert!(query.has_vector_ops());

        // Match with rank
        let query = QueryAst::new(SourceClause::Match(vec![Pattern::node(NodePattern::var(
            "n",
        ))]))
        .with_rank(RankClause {
            embedding: EmbeddingRef::Parameter("emb".to_string()),
            top_k: Some(10),
        });
        assert!(query.has_vector_ops());

        // Plain match
        let query = QueryAst::new(SourceClause::Match(vec![Pattern::node(NodePattern::var(
            "n",
        ))]));
        assert!(!query.has_vector_ops());
    }

    // =====================================================
    // Pattern Tests
    // =====================================================

    #[test]
    fn test_node_pattern_empty() {
        let node = NodePattern::empty();
        assert!(node.variable.is_none());
        assert!(node.label.is_none());
        assert!(node.properties.is_none());
    }

    #[test]
    fn test_node_pattern_var() {
        let node = NodePattern::var("n");
        assert_eq!(node.variable, Some("n".to_string()));
        assert!(node.label.is_none());
    }

    #[test]
    fn test_node_pattern_with_label() {
        let node = NodePattern::with_label("n", "Person");
        assert_eq!(node.variable, Some("n".to_string()));
        assert_eq!(node.label, Some("Person".to_string()));
    }

    #[test]
    fn test_node_pattern_with_properties() {
        let props = vec![(
            "name".to_string(),
            PropertyValue::String("Alice".to_string()),
        )];
        let node = NodePattern::with_label("n", "Person").with_properties(props.clone());
        assert_eq!(node.properties, Some(props));
    }

    #[test]
    fn test_relationship_pattern_outgoing() {
        let rel = RelationshipPattern::outgoing().with_type("KNOWS");
        assert_eq!(rel.direction, RelationshipDirection::Outgoing);
        assert_eq!(rel.rel_type, Some("KNOWS".to_string()));
    }

    #[test]
    fn test_relationship_pattern_incoming() {
        let rel = RelationshipPattern::incoming().with_type("FOLLOWS");
        assert_eq!(rel.direction, RelationshipDirection::Incoming);
        assert_eq!(rel.rel_type, Some("FOLLOWS".to_string()));
    }

    #[test]
    fn test_relationship_pattern_both() {
        let rel = RelationshipPattern::both();
        assert_eq!(rel.direction, RelationshipDirection::Both);
    }

    #[test]
    fn test_relationship_pattern_with_depth() {
        let rel = RelationshipPattern::outgoing()
            .with_type("KNOWS")
            .with_depth(DepthSpec::range(1, 3));
        assert_eq!(rel.depth, Some(DepthSpec::Range { min: 1, max: 3 }));
    }

    #[test]
    fn test_pattern_chain() {
        let pattern = Pattern::node(NodePattern::var("a"))
            .then(
                RelationshipPattern::outgoing().with_type("KNOWS"),
                NodePattern::var("b"),
            )
            .then(
                RelationshipPattern::outgoing().with_type("LIKES"),
                NodePattern::var("c"),
            );

        assert_eq!(pattern.elements.len(), 5); // a, -[:KNOWS]->, b, -[:LIKES]->, c
    }

    // =====================================================
    // Predicate Tests
    // =====================================================

    #[test]
    fn test_predicate_comparison() {
        let pred = PredicateExpr::eq(Expression::property("n", "age"), Expression::int(30));

        assert!(matches!(pred, PredicateExpr::Comparison { .. }));
    }

    #[test]
    fn test_predicate_and() {
        let p1 = PredicateExpr::eq(Expression::property("n", "age"), Expression::int(30));
        let p2 = PredicateExpr::eq(
            Expression::property("n", "name"),
            Expression::string("Alice"),
        );

        let combined = p1.and(p2);
        assert!(matches!(combined, PredicateExpr::And(_, _)));
    }

    #[test]
    fn test_predicate_or() {
        let p1 = PredicateExpr::eq(Expression::property("n", "age"), Expression::int(30));
        let p2 = PredicateExpr::eq(Expression::property("n", "age"), Expression::int(40));

        let combined = p1.or(p2);
        assert!(matches!(combined, PredicateExpr::Or(_, _)));
    }

    #[test]
    fn test_predicate_not() {
        let pred = PredicateExpr::eq(
            Expression::property("n", "active"),
            Expression::literal(PropertyValue::Bool(true)),
        );
        let negated = !pred;
        assert!(matches!(negated, PredicateExpr::Not(_)));
    }

    // =====================================================
    // Return Clause Tests
    // =====================================================

    #[test]
    fn test_return_clause() {
        let items = vec![
            ReturnItem::new(Expression::property("n", "name")),
            ReturnItem::new(Expression::property("n", "age")).with_alias("years"),
        ];
        let ret = ReturnClause::new(items);

        assert!(!ret.distinct);
        assert_eq!(ret.items.len(), 2);
        assert_eq!(ret.items[1].alias, Some("years".to_string()));
    }

    #[test]
    fn test_return_clause_distinct() {
        let items = vec![ReturnItem::new(Expression::property("n", "name"))];
        let ret = ReturnClause::new(items).distinct();
        assert!(ret.distinct);
    }

    // =====================================================
    // Order Clause Tests
    // =====================================================

    #[test]
    fn test_order_clause() {
        let items = vec![
            OrderItem::desc(Expression::property("n", "age")),
            OrderItem::asc(Expression::property("n", "name")),
        ];
        let order = OrderClause::new(items);

        assert_eq!(order.items.len(), 2);
        assert!(order.items[0].descending);
        assert!(!order.items[1].descending);
    }

    // =====================================================
    // Property Value Tests
    // =====================================================

    #[test]
    fn test_property_value_from() {
        let _v: PropertyValue = true.into();
        let _v: PropertyValue = 42i64.into();
        let _v: PropertyValue = 3.14f64.into();
        let _v: PropertyValue = "hello".into();
        let _v: PropertyValue = String::from("world").into();
    }

    // =====================================================
    // Temporal Clause Tests
    // =====================================================

    #[test]
    fn test_temporal_as_of() {
        let temporal = TemporalClause::AsOf {
            valid_time: TimestampLiteral::String("2024-01-15T10:00:00Z".to_string()),
            transaction_time: Some(TimestampLiteral::Integer(1705315200000)),
        };

        if let TemporalClause::AsOf {
            valid_time,
            transaction_time,
        } = temporal
        {
            assert!(matches!(valid_time, TimestampLiteral::String(_)));
            assert!(transaction_time.is_some());
        }
    }

    #[test]
    fn test_temporal_between() {
        let temporal = TemporalClause::Between {
            start: TimestampLiteral::String("2024-01-01".to_string()),
            end: TimestampLiteral::String("2024-12-31".to_string()),
        };

        assert!(matches!(temporal, TemporalClause::Between { .. }));
    }

    // =====================================================
    // Embedding Ref Tests
    // =====================================================

    #[test]
    fn test_embedding_ref_parameter() {
        let emb = EmbeddingRef::Parameter("embedding".to_string());
        assert!(matches!(emb, EmbeddingRef::Parameter(_)));
    }

    #[test]
    fn test_embedding_ref_literal() {
        let emb = EmbeddingRef::Literal(Arc::from([0.1f32, 0.2, 0.3].as_slice()));
        if let EmbeddingRef::Literal(arr) = emb {
            assert_eq!(arr.len(), 3);
        }
    }

    // =====================================================
    // Depth Spec Tests
    // =====================================================

    #[test]
    fn test_depth_spec() {
        assert_eq!(DepthSpec::one(), DepthSpec::Exact(1));
        assert_eq!(DepthSpec::exact(3), DepthSpec::Exact(3));
        assert_eq!(DepthSpec::range(1, 5), DepthSpec::Range { min: 1, max: 5 });
    }
}
