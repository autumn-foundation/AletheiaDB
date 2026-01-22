//! GQL Parser
//!
//! A recursive descent parser that converts tokenized GQL input into an AST.
//! The parser supports the full GQL grammar including MATCH patterns, vector
//! search operations, temporal clauses, and hybrid queries.

use std::sync::Arc;

use crate::index::vector::DistanceMetric;

use super::ast::*;
use super::lexer::{Lexer, LexerError, Token};

/// Error type for parser errors.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// Error message
    pub message: String,
    /// Position in the token stream
    pub position: usize,
    /// Expected token description
    pub expected: Option<String>,
    /// Found token
    pub found: Option<Token>,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Parse error at position {}: {}",
            self.position, self.message
        )?;
        if let Some(expected) = &self.expected {
            write!(f, " (expected {})", expected)?;
        }
        if let Some(found) = &self.found {
            write!(f, " (found {})", found)?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

impl From<LexerError> for ParseError {
    fn from(err: LexerError) -> Self {
        ParseError {
            message: err.message,
            position: err.position,
            expected: None,
            found: None,
        }
    }
}

/// A parser for the GQL query language.
pub struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    /// Parse a GQL query string into an AST.
    pub fn parse(input: &str) -> Result<QueryAst, ParseError> {
        let tokens = Lexer::tokenize(input)?;
        let mut parser = Parser {
            tokens,
            position: 0,
        };
        parser.parse_query()
    }

    /// Parse a complete query.
    fn parse_query(&mut self) -> Result<QueryAst, ParseError> {
        // Parse optional temporal clause
        let temporal = self.parse_temporal_clause()?;

        // Parse main source clause (MATCH or vector search)
        let source = self.parse_source_clause()?;

        // Build initial query
        let mut query = QueryAst::new(source);
        if let Some(t) = temporal {
            query = query.with_temporal(t);
        }

        // Parse optional RANK BY SIMILARITY clause
        if let Some(rank) = self.parse_rank_clause()? {
            query = query.with_rank(rank);
        }

        // Parse optional WHERE clause
        if let Some(where_clause) = self.parse_where_clause()? {
            query = query.with_where(where_clause);
        }

        // Parse optional RETURN clause
        if let Some(return_clause) = self.parse_return_clause()? {
            query = query.with_return(return_clause);
        }

        // Parse optional ORDER BY clause
        if let Some(order) = self.parse_order_clause()? {
            query = query.with_order(order);
        }

        // Parse optional SKIP clause
        if let Some(skip) = self.parse_skip_clause()? {
            query = query.with_skip(skip);
        }

        // Parse optional LIMIT clause
        if let Some(limit) = self.parse_limit_clause()? {
            query = query.with_limit(limit);
        }

        // Ensure we've consumed all tokens
        if !self.is_at_end() {
            return Err(self.error(
                "Unexpected tokens at end of query".to_string(),
                Some("end of query".to_string()),
            ));
        }

        Ok(query)
    }

    // =========================================================
    // Temporal Clause Parsing
    // =========================================================

    fn parse_temporal_clause(&mut self) -> Result<Option<TemporalClause>, ParseError> {
        if self.check(&Token::As) {
            self.advance(); // consume AS
            self.expect(&Token::Of)?; // expect OF
            return self.parse_as_of_clause().map(Some);
        }

        if self.check(&Token::Between) {
            self.advance(); // consume BETWEEN
            return self.parse_between_clause().map(Some);
        }

        Ok(None)
    }

    fn parse_as_of_clause(&mut self) -> Result<TemporalClause, ParseError> {
        let valid_time = self.parse_timestamp()?;

        let transaction_time = if self.check(&Token::Comma) {
            self.advance(); // consume comma
            Some(self.parse_timestamp()?)
        } else {
            None
        };

        Ok(TemporalClause::AsOf {
            valid_time,
            transaction_time,
        })
    }

    fn parse_between_clause(&mut self) -> Result<TemporalClause, ParseError> {
        let start = self.parse_timestamp()?;
        self.expect(&Token::And)?;
        let end = self.parse_timestamp()?;

        Ok(TemporalClause::Between { start, end })
    }

    fn parse_timestamp(&mut self) -> Result<TimestampLiteral, ParseError> {
        match self.current() {
            Some(Token::StringLiteral(s)) => {
                let ts = TimestampLiteral::String(s.clone());
                self.advance();
                Ok(ts)
            }
            Some(Token::IntegerLiteral(n)) => {
                let ts = TimestampLiteral::Integer(*n);
                self.advance();
                Ok(ts)
            }
            _ => Err(self.error(
                "Expected timestamp (string or integer)".to_string(),
                Some("timestamp".to_string()),
            )),
        }
    }

    // =========================================================
    // Source Clause Parsing
    // =========================================================

    fn parse_source_clause(&mut self) -> Result<SourceClause, ParseError> {
        if self.check(&Token::Match) {
            return self.parse_match_clause();
        }

        if self.check(&Token::Similar) {
            return self.parse_similar_clause();
        }

        if self.check(&Token::Find) {
            return self.parse_find_similar_clause();
        }

        Err(self.error(
            "Expected MATCH, SIMILAR, or FIND clause".to_string(),
            Some("MATCH, SIMILAR, or FIND".to_string()),
        ))
    }

    fn parse_match_clause(&mut self) -> Result<SourceClause, ParseError> {
        self.expect(&Token::Match)?;

        let mut patterns = vec![self.parse_pattern()?];

        // Parse additional patterns separated by commas
        while self.check(&Token::Comma) {
            self.advance();
            patterns.push(self.parse_pattern()?);
        }

        Ok(SourceClause::Match(patterns))
    }

    fn parse_similar_clause(&mut self) -> Result<SourceClause, ParseError> {
        self.expect(&Token::Similar)?;
        self.expect(&Token::To)?;

        let embedding = self.parse_embedding_ref()?;

        let metric = if self.check(&Token::Using) {
            self.advance();
            Some(self.parse_distance_metric()?)
        } else {
            None
        };

        let limit = if self.check(&Token::Limit) {
            self.advance();
            self.parse_usize()?
        } else {
            10 // default limit
        };

        Ok(SourceClause::VectorSearch {
            embedding,
            metric,
            limit,
        })
    }

    fn parse_find_similar_clause(&mut self) -> Result<SourceClause, ParseError> {
        self.expect(&Token::Find)?;
        self.expect(&Token::Similar)?;
        self.expect(&Token::To)?;
        self.expect(&Token::LeftParen)?;

        let node_ref = self.parse_node_ref()?;

        self.expect(&Token::RightParen)?;

        let limit = if self.check(&Token::Limit) {
            self.advance();
            self.parse_usize()?
        } else {
            10 // default limit
        };

        Ok(SourceClause::FindSimilar { node_ref, limit })
    }

    fn parse_embedding_ref(&mut self) -> Result<EmbeddingRef, ParseError> {
        match self.current() {
            Some(Token::Parameter(name)) => {
                let emb = EmbeddingRef::Parameter(name.clone());
                self.advance();
                Ok(emb)
            }
            Some(Token::LeftBracket) => {
                self.advance();
                let values = self.parse_float_list()?;
                if values.is_empty() {
                    return Err(self.error(
                        "Embedding array cannot be empty".to_string(),
                        Some("non-empty array".to_string()),
                    ));
                }
                self.expect(&Token::RightBracket)?;
                Ok(EmbeddingRef::Literal(Arc::from(values)))
            }
            _ => Err(self.error(
                "Expected parameter or embedding array".to_string(),
                Some("$parameter or [float, ...]".to_string()),
            )),
        }
    }

    fn parse_float_list(&mut self) -> Result<Vec<f32>, ParseError> {
        let mut values = Vec::new();

        loop {
            let value = match self.current() {
                Some(Token::FloatLiteral(f)) => *f as f32,
                Some(Token::IntegerLiteral(i)) => *i as f32,
                Some(Token::Dash) => {
                    self.advance();
                    match self.current() {
                        Some(Token::FloatLiteral(f)) => -(*f as f32),
                        Some(Token::IntegerLiteral(i)) => -(*i as f32),
                        _ => {
                            return Err(self.error(
                                "Expected number after '-'".to_string(),
                                Some("number".to_string()),
                            ));
                        }
                    }
                }
                _ => break,
            };
            self.advance();
            values.push(value);

            if !self.check(&Token::Comma) {
                break;
            }
            self.advance(); // consume comma
        }

        Ok(values)
    }

    fn parse_distance_metric(&mut self) -> Result<DistanceMetric, ParseError> {
        match self.current() {
            Some(Token::Cosine) => {
                self.advance();
                Ok(DistanceMetric::Cosine)
            }
            Some(Token::Euclidean) => {
                self.advance();
                Ok(DistanceMetric::Euclidean)
            }
            Some(Token::DotProduct) => {
                self.advance();
                Ok(DistanceMetric::DotProduct)
            }
            _ => Err(self.error(
                "Expected distance metric".to_string(),
                Some("COSINE, EUCLIDEAN, or DOT_PRODUCT".to_string()),
            )),
        }
    }

    fn parse_node_ref(&mut self) -> Result<NodeRef, ParseError> {
        match self.current() {
            Some(Token::Identifier(name)) => {
                let node_ref = NodeRef::Identifier(name.clone());
                self.advance();
                Ok(node_ref)
            }
            Some(Token::IntegerLiteral(id)) => {
                let node_ref = NodeRef::Id(*id as u64);
                self.advance();
                Ok(node_ref)
            }
            Some(Token::Parameter(name)) => {
                let node_ref = NodeRef::Parameter(name.clone());
                self.advance();
                Ok(node_ref)
            }
            _ => Err(self.error(
                "Expected node reference".to_string(),
                Some("identifier, integer, or parameter".to_string()),
            )),
        }
    }

    // =========================================================
    // Pattern Parsing
    // =========================================================

    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first_node = self.parse_node_pattern()?;
        let mut pattern = Pattern::node(first_node);

        // Parse relationship and node pairs
        while self.is_relationship_start() {
            let rel = self.parse_relationship_pattern()?;
            let node = self.parse_node_pattern()?;
            pattern = pattern.then(rel, node);
        }

        Ok(pattern)
    }

    fn is_relationship_start(&self) -> bool {
        matches!(self.current(), Some(Token::Dash) | Some(Token::LeftArrow))
    }

    fn parse_node_pattern(&mut self) -> Result<NodePattern, ParseError> {
        self.expect(&Token::LeftParen)?;

        let mut node = NodePattern::empty();

        // Parse optional variable
        if let Some(Token::Identifier(name)) = self.current() {
            node.variable = Some(name.clone());
            self.advance();
        }

        // Parse optional label
        if self.check(&Token::Colon) {
            self.advance();
            if let Some(Token::Identifier(label)) = self.current() {
                node.label = Some(label.clone());
                self.advance();
            } else {
                return Err(self.error(
                    "Expected label after ':'".to_string(),
                    Some("label".to_string()),
                ));
            }
        }

        // Parse optional properties
        if self.check(&Token::LeftBrace) {
            node.properties = Some(self.parse_property_map()?);
        }

        self.expect(&Token::RightParen)?;

        Ok(node)
    }

    fn parse_relationship_pattern(&mut self) -> Result<RelationshipPattern, ParseError> {
        let direction_start = if self.check(&Token::LeftArrow) {
            self.advance();
            RelationshipDirection::Incoming
        } else if self.check(&Token::Dash) {
            self.advance();
            RelationshipDirection::Both // Default, may change to Outgoing
        } else {
            return Err(self.error(
                "Expected relationship pattern".to_string(),
                Some("'-' or '<-'".to_string()),
            ));
        };

        // Parse relationship details inside brackets
        self.expect(&Token::LeftBracket)?;

        let mut rel = RelationshipPattern {
            variable: None,
            rel_type: None,
            direction: direction_start,
            depth: None,
        };

        // Parse optional variable
        if let Some(Token::Identifier(name)) = self.current() {
            // Check if followed by colon (label) or other
            let mut peek_clone = self.tokens[self.position..].iter();
            peek_clone.next(); // skip current
            if matches!(
                peek_clone.next(),
                Some(Token::Colon) | Some(Token::Star) | Some(Token::RightBracket)
            ) {
                rel.variable = Some(name.clone());
                self.advance();
            }
        }

        // Parse optional type
        if self.check(&Token::Colon) {
            self.advance();
            if let Some(Token::Identifier(label)) = self.current() {
                rel.rel_type = Some(label.clone());
                self.advance();
            }
        }

        // Parse optional depth specification
        if self.check(&Token::Star) {
            rel.depth = Some(self.parse_depth_spec()?);
        }

        self.expect(&Token::RightBracket)?;

        // Parse end arrow to determine final direction
        // Pattern combinations:
        //   -[]->  = Outgoing
        //   <-[]-  = Incoming
        //   -[]-   = Both
        //   <-[]-> = Both (bidirectional)
        if self.check(&Token::Arrow) {
            self.advance();
            if direction_start == RelationshipDirection::Incoming {
                // <-[]-> is bidirectional
                rel.direction = RelationshipDirection::Both;
            } else {
                // -[]-> is outgoing
                rel.direction = RelationshipDirection::Outgoing;
            }
        } else if self.check(&Token::Dash) {
            self.advance();
            if direction_start == RelationshipDirection::Incoming {
                // <-[]- stays as Incoming
                rel.direction = RelationshipDirection::Incoming;
            } else {
                // -[]- is both
                rel.direction = RelationshipDirection::Both;
            }
        }
        // If no ending marker, keep the start direction

        Ok(rel)
    }

    fn parse_depth_spec(&mut self) -> Result<DepthSpec, ParseError> {
        self.expect(&Token::Star)?;

        // Check for range or exact
        match self.current() {
            Some(Token::IntegerLiteral(n)) => {
                let n_val = *n;
                if n_val < 0 {
                    return Err(self.error(
                        format!("Depth must be non-negative, got {}", n_val),
                        Some("non-negative integer".to_string()),
                    ));
                }
                let min = n_val as usize;
                self.advance();

                if self.check(&Token::Dot) {
                    self.advance();
                    self.expect(&Token::Dot)?;

                    if let Some(Token::IntegerLiteral(m)) = self.current() {
                        let m_val = *m;
                        if m_val < 0 {
                            return Err(self.error(
                                format!("Depth must be non-negative, got {}", m_val),
                                Some("non-negative integer".to_string()),
                            ));
                        }
                        let max = m_val as usize;
                        if min > max {
                            return Err(self.error(
                                format!("Invalid depth range: min ({}) > max ({})", min, max),
                                Some("valid range".to_string()),
                            ));
                        }
                        self.advance();
                        Ok(DepthSpec::Range { min, max })
                    } else {
                        // *n.. is unbounded max, use Variable with min hops
                        // Since we can't express min with Variable, use a large max
                        Ok(DepthSpec::Range {
                            min,
                            max: usize::MAX / 2, // Use half max to avoid overflow issues
                        })
                    }
                } else {
                    Ok(DepthSpec::Exact(min))
                }
            }
            Some(Token::Dot) => {
                self.advance();
                self.expect(&Token::Dot)?;

                if let Some(Token::IntegerLiteral(n)) = self.current() {
                    let n_val = *n;
                    if n_val < 0 {
                        return Err(self.error(
                            format!("Depth must be non-negative, got {}", n_val),
                            Some("non-negative integer".to_string()),
                        ));
                    }
                    let max = n_val as usize;
                    self.advance();
                    Ok(DepthSpec::Max(max))
                } else {
                    Ok(DepthSpec::Variable)
                }
            }
            _ => Ok(DepthSpec::Variable),
        }
    }

    fn parse_property_map(&mut self) -> Result<PropertyMap, ParseError> {
        self.expect(&Token::LeftBrace)?;

        let mut props = Vec::new();

        if !self.check(&Token::RightBrace) {
            loop {
                let key = self.parse_identifier()?;
                self.expect(&Token::Colon)?;
                let value = self.parse_property_value()?;
                props.push((key, value));

                if !self.check(&Token::Comma) {
                    break;
                }
                self.advance();
            }
        }

        self.expect(&Token::RightBrace)?;
        Ok(props)
    }

    fn parse_property_value(&mut self) -> Result<PropertyValue, ParseError> {
        match self.current() {
            Some(Token::Null) => {
                self.advance();
                Ok(PropertyValue::Null)
            }
            Some(Token::True) => {
                self.advance();
                Ok(PropertyValue::Bool(true))
            }
            Some(Token::False) => {
                self.advance();
                Ok(PropertyValue::Bool(false))
            }
            Some(Token::IntegerLiteral(n)) => {
                let v = PropertyValue::Int(*n);
                self.advance();
                Ok(v)
            }
            Some(Token::FloatLiteral(f)) => {
                let v = PropertyValue::Float(*f);
                self.advance();
                Ok(v)
            }
            Some(Token::StringLiteral(s)) => {
                let v = PropertyValue::String(s.clone());
                self.advance();
                Ok(v)
            }
            Some(Token::Parameter(p)) => {
                let v = PropertyValue::Parameter(p.clone());
                self.advance();
                Ok(v)
            }
            Some(Token::Dash) => {
                self.advance();
                match self.current() {
                    Some(Token::IntegerLiteral(n)) => {
                        let v = PropertyValue::Int(-*n);
                        self.advance();
                        Ok(v)
                    }
                    Some(Token::FloatLiteral(f)) => {
                        let v = PropertyValue::Float(-*f);
                        self.advance();
                        Ok(v)
                    }
                    _ => Err(self.error(
                        "Expected number after '-'".to_string(),
                        Some("number".to_string()),
                    )),
                }
            }
            _ => Err(self.error(
                "Expected property value".to_string(),
                Some("value".to_string()),
            )),
        }
    }

    // =========================================================
    // RANK BY SIMILARITY Clause
    // =========================================================

    fn parse_rank_clause(&mut self) -> Result<Option<RankClause>, ParseError> {
        if !self.check(&Token::Rank) {
            return Ok(None);
        }

        self.advance(); // RANK
        self.expect(&Token::By)?;
        self.expect(&Token::Similarity)?;
        self.expect(&Token::To)?;

        let embedding = self.parse_embedding_ref()?;

        let top_k = if self.check(&Token::Top) {
            self.advance();
            Some(self.parse_usize()?)
        } else {
            None
        };

        Ok(Some(RankClause { embedding, top_k }))
    }

    // =========================================================
    // WHERE Clause
    // =========================================================

    fn parse_where_clause(&mut self) -> Result<Option<WhereClause>, ParseError> {
        if !self.check(&Token::Where) {
            return Ok(None);
        }

        self.advance(); // WHERE
        let predicate = self.parse_predicate()?;

        Ok(Some(WhereClause { predicate }))
    }

    fn parse_predicate(&mut self) -> Result<PredicateExpr, ParseError> {
        self.parse_or_predicate()
    }

    fn parse_or_predicate(&mut self) -> Result<PredicateExpr, ParseError> {
        let mut left = self.parse_and_predicate()?;

        while self.check(&Token::Or) {
            self.advance();
            let right = self.parse_and_predicate()?;
            left = PredicateExpr::Or(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    fn parse_and_predicate(&mut self) -> Result<PredicateExpr, ParseError> {
        let mut left = self.parse_not_predicate()?;

        while self.check(&Token::And) {
            self.advance();
            let right = self.parse_not_predicate()?;
            left = PredicateExpr::And(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    fn parse_not_predicate(&mut self) -> Result<PredicateExpr, ParseError> {
        if self.check(&Token::Not) {
            self.advance();
            let pred = self.parse_not_predicate()?;
            return Ok(PredicateExpr::Not(Box::new(pred)));
        }

        self.parse_primary_predicate()
    }

    fn parse_primary_predicate(&mut self) -> Result<PredicateExpr, ParseError> {
        // Parenthesized predicate
        if self.check(&Token::LeftParen) {
            self.advance();
            let pred = self.parse_predicate()?;
            self.expect(&Token::RightParen)?;
            return Ok(PredicateExpr::Grouped(Box::new(pred)));
        }

        // EXISTS(n.prop)
        if self.check(&Token::Exists) {
            self.advance();
            self.expect(&Token::LeftParen)?;
            let prop = self.parse_property_access()?;
            self.expect(&Token::RightParen)?;
            return Ok(PredicateExpr::Exists(prop));
        }

        // Property-based predicates
        let expr = self.parse_expression()?;

        // IS [NOT] NULL
        if self.check(&Token::Is) {
            self.advance();
            let is_not = if self.check(&Token::Not) {
                self.advance();
                true
            } else {
                false
            };
            self.expect(&Token::Null)?;

            if let Expression::Property(prop) = expr {
                return Ok(if is_not {
                    PredicateExpr::IsNotNull(prop)
                } else {
                    PredicateExpr::IsNull(prop)
                });
            } else {
                return Err(self.error(
                    "IS NULL requires property access".to_string(),
                    Some("property".to_string()),
                ));
            }
        }

        // CONTAINS, STARTS WITH, ENDS WITH
        if self.check(&Token::Contains) {
            self.advance();
            let substring = self.parse_string()?;
            if let Expression::Property(prop) = expr {
                return Ok(PredicateExpr::Contains {
                    property: prop,
                    substring,
                });
            } else {
                return Err(self.error(
                    "CONTAINS requires a property expression".to_string(),
                    Some("property.name CONTAINS 'value'".to_string()),
                ));
            }
        }

        if self.check(&Token::Starts) {
            self.advance();
            self.expect(&Token::With)?;
            let prefix = self.parse_string()?;
            if let Expression::Property(prop) = expr {
                return Ok(PredicateExpr::StartsWith {
                    property: prop,
                    prefix,
                });
            } else {
                return Err(self.error(
                    "STARTS WITH requires a property expression".to_string(),
                    Some("property.name STARTS WITH 'value'".to_string()),
                ));
            }
        }

        if self.check(&Token::Ends) {
            self.advance();
            self.expect(&Token::With)?;
            let suffix = self.parse_string()?;
            if let Expression::Property(prop) = expr {
                return Ok(PredicateExpr::EndsWith {
                    property: prop,
                    suffix,
                });
            } else {
                return Err(self.error(
                    "ENDS WITH requires a property expression".to_string(),
                    Some("property.name ENDS WITH 'value'".to_string()),
                ));
            }
        }

        // IN [list]
        if self.check(&Token::In) {
            self.advance();
            self.expect(&Token::LeftBracket)?;
            let mut values = Vec::new();
            if !self.check(&Token::RightBracket) {
                loop {
                    values.push(self.parse_property_value()?);
                    if !self.check(&Token::Comma) {
                        break;
                    }
                    self.advance();
                }
            }
            self.expect(&Token::RightBracket)?;
            if let Expression::Property(prop) = expr {
                return Ok(PredicateExpr::In {
                    property: prop,
                    values,
                });
            } else {
                return Err(self.error(
                    "IN requires a property expression".to_string(),
                    Some("property.name IN [...]".to_string()),
                ));
            }
        }

        // Comparison operators
        let op = match self.current() {
            Some(Token::Eq) => ComparisonOp::Eq,
            Some(Token::Ne) => ComparisonOp::Ne,
            Some(Token::Lt) => ComparisonOp::Lt,
            Some(Token::Le) => ComparisonOp::Le,
            Some(Token::Gt) => ComparisonOp::Gt,
            Some(Token::Ge) => ComparisonOp::Ge,
            _ => {
                return Err(self.error(
                    "Expected comparison operator".to_string(),
                    Some("=, <>, <, <=, >, >=".to_string()),
                ));
            }
        };
        self.advance();

        let right = self.parse_expression()?;

        Ok(PredicateExpr::Comparison {
            left: expr,
            op,
            right,
        })
    }

    fn parse_expression(&mut self) -> Result<Expression, ParseError> {
        match self.current() {
            Some(Token::Identifier(_)) => {
                // Could be property access (n.prop) or just identifier (n)
                let ident = self.parse_identifier()?;
                if self.check(&Token::Dot) {
                    self.advance();
                    let prop = self.parse_identifier()?;
                    Ok(Expression::Property(PropertyAccess {
                        variable: ident,
                        property: prop,
                    }))
                } else {
                    // Just an identifier - a variable reference
                    Ok(Expression::Identifier(ident))
                }
            }
            Some(Token::Parameter(p)) => {
                let param = p.clone();
                self.advance();
                Ok(Expression::Parameter(param))
            }
            Some(Token::StringLiteral(s)) => {
                let val = PropertyValue::String(s.clone());
                self.advance();
                Ok(Expression::Literal(val))
            }
            Some(Token::IntegerLiteral(n)) => {
                let val = PropertyValue::Int(*n);
                self.advance();
                Ok(Expression::Literal(val))
            }
            Some(Token::FloatLiteral(f)) => {
                let val = PropertyValue::Float(*f);
                self.advance();
                Ok(Expression::Literal(val))
            }
            Some(Token::True) => {
                self.advance();
                Ok(Expression::Literal(PropertyValue::Bool(true)))
            }
            Some(Token::False) => {
                self.advance();
                Ok(Expression::Literal(PropertyValue::Bool(false)))
            }
            Some(Token::Null) => {
                self.advance();
                Ok(Expression::Literal(PropertyValue::Null))
            }
            Some(Token::Dash) => {
                self.advance();
                match self.current() {
                    Some(Token::IntegerLiteral(n)) => {
                        let val = PropertyValue::Int(-*n);
                        self.advance();
                        Ok(Expression::Literal(val))
                    }
                    Some(Token::FloatLiteral(f)) => {
                        let val = PropertyValue::Float(-*f);
                        self.advance();
                        Ok(Expression::Literal(val))
                    }
                    _ => Err(self.error(
                        "Expected number after '-'".to_string(),
                        Some("number".to_string()),
                    )),
                }
            }
            _ => Err(self.error(
                "Expected expression".to_string(),
                Some("identifier, literal, or parameter".to_string()),
            )),
        }
    }

    fn parse_property_access(&mut self) -> Result<PropertyAccess, ParseError> {
        let variable = self.parse_identifier()?;
        self.expect(&Token::Dot)?;
        let property = self.parse_identifier()?;
        Ok(PropertyAccess { variable, property })
    }

    // =========================================================
    // RETURN Clause
    // =========================================================

    fn parse_return_clause(&mut self) -> Result<Option<ReturnClause>, ParseError> {
        if !self.check(&Token::Return) {
            return Ok(None);
        }

        self.advance(); // RETURN

        let distinct = if self.check(&Token::Distinct) {
            self.advance();
            true
        } else {
            false
        };

        // Check for COUNT
        if self.check(&Token::Count) {
            self.advance();
            self.expect(&Token::LeftParen)?;
            let arg = if self.check(&Token::Star) {
                self.advance();
                // Represent COUNT(*) with a special identifier
                Expression::Identifier("*".to_string())
            } else {
                self.parse_expression()?
            };
            self.expect(&Token::RightParen)?;

            let items = vec![ReturnItem::new(Expression::FunctionCall {
                name: "COUNT".to_string(),
                args: vec![arg],
            })];
            return Ok(Some(ReturnClause { items, distinct }));
        }

        let mut items = vec![self.parse_return_item()?];

        while self.check(&Token::Comma) {
            self.advance();
            items.push(self.parse_return_item()?);
        }

        Ok(Some(ReturnClause { items, distinct }))
    }

    fn parse_return_item(&mut self) -> Result<ReturnItem, ParseError> {
        let expr = self.parse_expression()?;

        let alias = if self.check(&Token::As) {
            self.advance();
            Some(self.parse_identifier()?)
        } else {
            None
        };

        Ok(ReturnItem {
            expression: expr,
            alias,
        })
    }

    // =========================================================
    // ORDER BY Clause
    // =========================================================

    fn parse_order_clause(&mut self) -> Result<Option<OrderClause>, ParseError> {
        if !self.check(&Token::Order) {
            return Ok(None);
        }

        self.advance(); // ORDER
        self.expect(&Token::By)?;

        let mut items = vec![self.parse_order_item()?];

        while self.check(&Token::Comma) {
            self.advance();
            items.push(self.parse_order_item()?);
        }

        Ok(Some(OrderClause { items }))
    }

    fn parse_order_item(&mut self) -> Result<OrderItem, ParseError> {
        let expr = self.parse_expression()?;

        let descending = if self.check(&Token::Desc) {
            self.advance();
            true
        } else if self.check(&Token::Asc) {
            self.advance();
            false
        } else {
            false // default ASC
        };

        Ok(OrderItem {
            expression: expr,
            descending,
        })
    }

    // =========================================================
    // SKIP/LIMIT Clauses
    // =========================================================

    fn parse_skip_clause(&mut self) -> Result<Option<usize>, ParseError> {
        if !self.check(&Token::Skip) {
            return Ok(None);
        }

        self.advance();
        Ok(Some(self.parse_usize()?))
    }

    fn parse_limit_clause(&mut self) -> Result<Option<usize>, ParseError> {
        if !self.check(&Token::Limit) {
            return Ok(None);
        }

        self.advance();
        Ok(Some(self.parse_usize()?))
    }

    // =========================================================
    // Utility Methods
    // =========================================================

    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn check(&self, expected: &Token) -> bool {
        match (self.current(), expected) {
            (Some(current), expected) => {
                std::mem::discriminant(current) == std::mem::discriminant(expected)
            }
            _ => false,
        }
    }

    fn advance(&mut self) {
        if !self.is_at_end() {
            self.position += 1;
        }
    }

    fn is_at_end(&self) -> bool {
        matches!(self.current(), Some(Token::Eof) | None)
    }

    fn expect(&mut self, expected: &Token) -> Result<(), ParseError> {
        if self.check(expected) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("Expected {}", expected), Some(expected.to_string())))
        }
    }

    fn parse_identifier(&mut self) -> Result<String, ParseError> {
        match self.current() {
            Some(Token::Identifier(name)) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(self.error(
                "Expected identifier".to_string(),
                Some("identifier".to_string()),
            )),
        }
    }

    fn parse_usize(&mut self) -> Result<usize, ParseError> {
        match self.current() {
            Some(Token::IntegerLiteral(n)) => {
                let n = *n;
                if n < 0 {
                    return Err(self.error(
                        format!("Expected non-negative integer, got {}", n),
                        Some("non-negative integer".to_string()),
                    ));
                }
                self.advance();
                // Safe conversion: on 32-bit systems, values > usize::MAX are clamped.
                // This is acceptable for SKIP/LIMIT as such large values are impractical.
                let result = usize::try_from(n).unwrap_or(usize::MAX);
                Ok(result)
            }
            _ => Err(self.error(
                "Expected non-negative integer".to_string(),
                Some("non-negative integer".to_string()),
            )),
        }
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        match self.current() {
            Some(Token::StringLiteral(s)) => {
                let s = s.clone();
                self.advance();
                Ok(s)
            }
            _ => Err(self.error("Expected string".to_string(), Some("string".to_string()))),
        }
    }

    fn error(&self, message: String, expected: Option<String>) -> ParseError {
        ParseError {
            message,
            position: self.position,
            expected,
            found: self.current().cloned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================
    // Basic Query Tests
    // =====================================================

    #[test]
    fn test_parse_simple_match() {
        let query = Parser::parse("MATCH (n) RETURN n").unwrap();

        assert!(matches!(query.source, SourceClause::Match(_)));
        assert!(query.return_clause.is_some());
    }

    #[test]
    fn test_parse_match_with_label() {
        let query = Parser::parse("MATCH (n:Person) RETURN n").unwrap();

        if let SourceClause::Match(patterns) = &query.source {
            assert_eq!(patterns.len(), 1);
            if let PatternElement::Node(node) = &patterns[0].elements[0] {
                assert_eq!(node.variable, Some("n".to_string()));
                assert_eq!(node.label, Some("Person".to_string()));
            } else {
                panic!("Expected node pattern");
            }
        } else {
            panic!("Expected MATCH clause");
        }
    }

    #[test]
    fn test_parse_match_with_properties() {
        let query = Parser::parse("MATCH (n:Person {name: 'Alice'}) RETURN n").unwrap();

        if let SourceClause::Match(patterns) = &query.source
            && let PatternElement::Node(node) = &patterns[0].elements[0]
        {
            assert!(node.properties.is_some());
            let props = node.properties.as_ref().unwrap();
            assert_eq!(props.len(), 1);
            assert_eq!(props[0].0, "name");
            assert_eq!(props[0].1, PropertyValue::String("Alice".to_string()));
        }
    }

    // =====================================================
    // Relationship Tests
    // =====================================================

    #[test]
    fn test_parse_outgoing_relationship() {
        let query = Parser::parse("MATCH (a)-[:KNOWS]->(b) RETURN a, b").unwrap();

        if let SourceClause::Match(patterns) = &query.source {
            assert_eq!(patterns[0].elements.len(), 3); // a, rel, b
            if let PatternElement::Relationship(rel) = &patterns[0].elements[1] {
                assert_eq!(rel.direction, RelationshipDirection::Outgoing);
                assert_eq!(rel.rel_type, Some("KNOWS".to_string()));
            }
        }
    }

    #[test]
    fn test_parse_incoming_relationship() {
        let query = Parser::parse("MATCH (a)<-[:FOLLOWS]-(b) RETURN a, b").unwrap();

        if let SourceClause::Match(patterns) = &query.source
            && let PatternElement::Relationship(rel) = &patterns[0].elements[1]
        {
            assert_eq!(rel.direction, RelationshipDirection::Incoming);
            assert_eq!(rel.rel_type, Some("FOLLOWS".to_string()));
        }
    }

    #[test]
    fn test_parse_bidirectional_relationship() {
        let query = Parser::parse("MATCH (a)-[:RELATED]-(b) RETURN a, b").unwrap();

        if let SourceClause::Match(patterns) = &query.source
            && let PatternElement::Relationship(rel) = &patterns[0].elements[1]
        {
            assert_eq!(rel.direction, RelationshipDirection::Both);
        }
    }

    #[test]
    fn test_parse_variable_length_path() {
        let query = Parser::parse("MATCH (a)-[:KNOWS*1..3]->(b) RETURN b").unwrap();

        if let SourceClause::Match(patterns) = &query.source
            && let PatternElement::Relationship(rel) = &patterns[0].elements[1]
        {
            assert_eq!(rel.depth, Some(DepthSpec::Range { min: 1, max: 3 }));
        }
    }

    #[test]
    fn test_parse_exact_depth_path() {
        let query = Parser::parse("MATCH (a)-[:KNOWS*2]->(b) RETURN b").unwrap();

        if let SourceClause::Match(patterns) = &query.source
            && let PatternElement::Relationship(rel) = &patterns[0].elements[1]
        {
            assert_eq!(rel.depth, Some(DepthSpec::Exact(2)));
        }
    }

    // =====================================================
    // Vector Search Tests
    // =====================================================

    #[test]
    fn test_parse_similar_to_parameter() {
        let query = Parser::parse("SIMILAR TO $embedding LIMIT 10").unwrap();

        if let SourceClause::VectorSearch {
            embedding, limit, ..
        } = &query.source
        {
            assert!(matches!(embedding, EmbeddingRef::Parameter(_)));
            assert_eq!(*limit, 10);
        } else {
            panic!("Expected VectorSearch clause");
        }
    }

    #[test]
    fn test_parse_similar_to_literal() {
        let query = Parser::parse("SIMILAR TO [0.1, 0.2, 0.3] LIMIT 5").unwrap();

        if let SourceClause::VectorSearch {
            embedding, limit, ..
        } = &query.source
        {
            if let EmbeddingRef::Literal(arr) = embedding {
                assert_eq!(arr.len(), 3);
                assert!((arr[0] - 0.1).abs() < 0.001);
            }
            assert_eq!(*limit, 5);
        }
    }

    #[test]
    fn test_parse_similar_with_metric() {
        let query = Parser::parse("SIMILAR TO $emb USING COSINE LIMIT 10").unwrap();

        if let SourceClause::VectorSearch { metric, .. } = &query.source {
            assert_eq!(*metric, Some(DistanceMetric::Cosine));
        }
    }

    #[test]
    fn test_parse_find_similar() {
        let query = Parser::parse("FIND SIMILAR TO (node_id) LIMIT 10").unwrap();

        assert!(matches!(query.source, SourceClause::FindSimilar { .. }));
    }

    // =====================================================
    // Temporal Tests
    // =====================================================

    #[test]
    fn test_parse_as_of_string() {
        let query = Parser::parse("AS OF '2024-01-15T10:00:00Z' MATCH (n) RETURN n").unwrap();

        assert!(query.temporal.is_some());
        if let Some(TemporalClause::AsOf {
            valid_time,
            transaction_time,
        }) = &query.temporal
        {
            assert!(matches!(valid_time, TimestampLiteral::String(_)));
            assert!(transaction_time.is_none());
        }
    }

    #[test]
    fn test_parse_as_of_bitemporal() {
        let query = Parser::parse("AS OF '2024-01-15', 1705315200000 MATCH (n) RETURN n").unwrap();

        if let Some(TemporalClause::AsOf {
            valid_time,
            transaction_time,
        }) = &query.temporal
        {
            assert!(matches!(valid_time, TimestampLiteral::String(_)));
            assert!(transaction_time.is_some());
            assert!(matches!(
                transaction_time.as_ref().unwrap(),
                TimestampLiteral::Integer(_)
            ));
        }
    }

    #[test]
    fn test_parse_between() {
        let query =
            Parser::parse("BETWEEN '2024-01-01' AND '2024-12-31' MATCH (n) RETURN n").unwrap();

        assert!(matches!(
            query.temporal,
            Some(TemporalClause::Between { .. })
        ));
    }

    // =====================================================
    // WHERE Clause Tests
    // =====================================================

    #[test]
    fn test_parse_where_equality() {
        let query = Parser::parse("MATCH (n) WHERE n.name = 'Alice' RETURN n").unwrap();

        assert!(query.where_clause.is_some());
        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Comparison { .. }));
        }
    }

    #[test]
    fn test_parse_where_comparison() {
        let query = Parser::parse("MATCH (n) WHERE n.age > 18 RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause
            && let PredicateExpr::Comparison { op, .. } = predicate
        {
            assert_eq!(*op, ComparisonOp::Gt);
        }
    }

    #[test]
    fn test_parse_where_and() {
        let query =
            Parser::parse("MATCH (n) WHERE n.age > 18 AND n.name = 'Alice' RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::And(_, _)));
        }
    }

    #[test]
    fn test_parse_where_or() {
        let query = Parser::parse("MATCH (n) WHERE n.age = 20 OR n.age = 30 RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Or(_, _)));
        }
    }

    #[test]
    fn test_parse_where_not() {
        let query = Parser::parse("MATCH (n) WHERE NOT n.active = true RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Not(_)));
        }
    }

    #[test]
    fn test_parse_where_is_null() {
        let query = Parser::parse("MATCH (n) WHERE n.email IS NULL RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::IsNull(_)));
        }
    }

    #[test]
    fn test_parse_where_is_not_null() {
        let query = Parser::parse("MATCH (n) WHERE n.email IS NOT NULL RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::IsNotNull(_)));
        }
    }

    #[test]
    fn test_parse_where_contains() {
        let query = Parser::parse("MATCH (n) WHERE n.name CONTAINS 'Ali' RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::Contains { .. }));
        }
    }

    #[test]
    fn test_parse_where_starts_with() {
        let query = Parser::parse("MATCH (n) WHERE n.name STARTS WITH 'A' RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::StartsWith { .. }));
        }
    }

    #[test]
    fn test_parse_where_in() {
        let query = Parser::parse("MATCH (n) WHERE n.age IN [20, 30, 40] RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause
            && let PredicateExpr::In { values, .. } = predicate
        {
            assert_eq!(values.len(), 3);
        }
    }

    #[test]
    fn test_parse_where_grouped() {
        let query =
            Parser::parse("MATCH (n) WHERE (n.a = 1 OR n.b = 2) AND n.c = 3 RETURN n").unwrap();

        if let Some(WhereClause { predicate }) = &query.where_clause {
            assert!(matches!(predicate, PredicateExpr::And(_, _)));
        }
    }

    // =====================================================
    // RANK BY SIMILARITY Tests
    // =====================================================

    #[test]
    fn test_parse_rank_by_similarity() {
        let query = Parser::parse(
            "MATCH (a:Person)-[:KNOWS]->(b) RANK BY SIMILARITY TO $embedding TOP 10 RETURN b",
        )
        .unwrap();

        assert!(query.rank.is_some());
        if let Some(rank) = &query.rank {
            assert!(matches!(rank.embedding, EmbeddingRef::Parameter(_)));
            assert_eq!(rank.top_k, Some(10));
        }
    }

    // =====================================================
    // RETURN Clause Tests
    // =====================================================

    #[test]
    fn test_parse_return_multiple() {
        let query = Parser::parse("MATCH (n) RETURN n.name, n.age").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items.len(), 2);
        }
    }

    #[test]
    fn test_parse_return_with_alias() {
        let query = Parser::parse("MATCH (n) RETURN n.name AS name, n.age AS years").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items[0].alias, Some("name".to_string()));
            assert_eq!(ret.items[1].alias, Some("years".to_string()));
        }
    }

    #[test]
    fn test_parse_return_distinct() {
        let query = Parser::parse("MATCH (n) RETURN DISTINCT n.name").unwrap();

        if let Some(ret) = &query.return_clause {
            assert!(ret.distinct);
        }
    }

    #[test]
    fn test_parse_return_count() {
        let query = Parser::parse("MATCH (n) RETURN COUNT(*)").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items.len(), 1);
            if let Expression::FunctionCall { name, args } = &ret.items[0].expression {
                assert_eq!(name, "COUNT");
                assert_eq!(args.len(), 1);
                // COUNT(*) should have "*" as the argument
                assert!(matches!(&args[0], Expression::Identifier(s) if s == "*"));
            } else {
                panic!("Expected FunctionCall");
            }
        } else {
            panic!("Expected return clause");
        }
    }

    #[test]
    fn test_parse_return_count_expression() {
        let query = Parser::parse("MATCH (n) RETURN COUNT(n)").unwrap();

        if let Some(ret) = &query.return_clause {
            assert_eq!(ret.items.len(), 1);
            if let Expression::FunctionCall { name, args } = &ret.items[0].expression {
                assert_eq!(name, "COUNT");
                assert_eq!(args.len(), 1);
                // COUNT(n) should have "n" as the argument
                assert!(matches!(&args[0], Expression::Identifier(s) if s == "n"));
            } else {
                panic!("Expected FunctionCall");
            }
        } else {
            panic!("Expected return clause");
        }
    }

    // =====================================================
    // ORDER BY Tests
    // =====================================================

    #[test]
    fn test_parse_order_by() {
        let query = Parser::parse("MATCH (n) RETURN n ORDER BY n.age DESC").unwrap();

        assert!(query.order.is_some());
        if let Some(order) = &query.order {
            assert_eq!(order.items.len(), 1);
            assert!(order.items[0].descending);
        }
    }

    #[test]
    fn test_parse_order_by_multiple() {
        let query = Parser::parse("MATCH (n) RETURN n ORDER BY n.age DESC, n.name ASC").unwrap();

        if let Some(order) = &query.order {
            assert_eq!(order.items.len(), 2);
            assert!(order.items[0].descending);
            assert!(!order.items[1].descending);
        }
    }

    // =====================================================
    // SKIP/LIMIT Tests
    // =====================================================

    #[test]
    fn test_parse_limit() {
        let query = Parser::parse("MATCH (n) RETURN n LIMIT 10").unwrap();
        assert_eq!(query.limit, Some(10));
    }

    #[test]
    fn test_parse_skip() {
        let query = Parser::parse("MATCH (n) RETURN n SKIP 5 LIMIT 10").unwrap();
        assert_eq!(query.skip, Some(5));
        assert_eq!(query.limit, Some(10));
    }

    // =====================================================
    // Hybrid Query Tests
    // =====================================================

    #[test]
    fn test_parse_full_hybrid_query() {
        let query = Parser::parse(
            "AS OF '2024-01-01' MATCH (a:Person)-[:KNOWS]->(b) \
             RANK BY SIMILARITY TO $emb TOP 10 \
             WHERE b.age > 18 \
             RETURN b.name, b.age \
             ORDER BY b.age DESC \
             SKIP 5 LIMIT 10",
        )
        .unwrap();

        assert!(query.temporal.is_some());
        assert!(matches!(query.source, SourceClause::Match(_)));
        assert!(query.rank.is_some());
        assert!(query.where_clause.is_some());
        assert!(query.return_clause.is_some());
        assert!(query.order.is_some());
        assert_eq!(query.skip, Some(5));
        assert_eq!(query.limit, Some(10));
    }

    // =====================================================
    // Error Tests
    // =====================================================

    #[test]
    fn test_parse_error_missing_source() {
        let result = Parser::parse("WHERE n.age > 18");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_invalid_pattern() {
        let result = Parser::parse("MATCH n RETURN n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_unclosed_paren() {
        let result = Parser::parse("MATCH (n:Person RETURN n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_missing_label() {
        let result = Parser::parse("MATCH (n:) RETURN n");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_negative_limit() {
        let result = Parser::parse("MATCH (n) RETURN n LIMIT -10");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("non-negative"));
    }

    #[test]
    fn test_parse_error_negative_skip() {
        let result = Parser::parse("MATCH (n) RETURN n SKIP -5");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_error_empty_embedding() {
        let result = Parser::parse("SIMILAR TO [] LIMIT 10");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("cannot be empty"));
    }

    #[test]
    fn test_parse_error_negative_depth() {
        let result = Parser::parse("MATCH (a)-[:KNOWS*-5]->(b) RETURN b");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("non-negative"));
    }

    #[test]
    fn test_parse_error_invalid_depth_range() {
        let result = Parser::parse("MATCH (a)-[:KNOWS*10..5]->(b) RETURN b");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Invalid depth range"));
    }

    #[test]
    fn test_parse_error_contains_on_non_property() {
        let result = Parser::parse("MATCH (n) WHERE 'hello' CONTAINS 'ell' RETURN n");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("property expression"));
    }

    #[test]
    fn test_parse_error_in_on_non_property() {
        let result = Parser::parse("MATCH (n) WHERE 5 IN [1, 2, 3] RETURN n");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("property expression"));
    }
}
