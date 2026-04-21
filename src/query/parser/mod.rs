//! # AQL Parser
//!
//! A recursive descent parser that converts tokenized AQL (Aletheia Query Language) input into an
//! Abstract Syntax Tree (AST).
//!
//! This module is the entry point for the query compilation pipeline. It takes a raw string query
//! and transforms it into a structured representation that the query planner can optimize and execute.
//!
//! ## Grammar Overview
//!
//! The parser supports a Cypher-like syntax with AletheiaDB-specific extensions for:
//!
//! - **Graph Pattern Matching**: `MATCH (n:Person)-[:KNOWS]->(m)`
//! - **Vector Search**: `SIMILAR TO $embedding` or `RANK BY SIMILARITY`
//! - **Temporal Queries**: `AS OF '2024-01-01'` or `BETWEEN t1 AND t2`
//! - **Hybrid Queries**: Combining graph traversal, vector similarity, and temporal filters.
//!
//! ## Example: The "Hero's Journey" Query
//!
//! This example demonstrates a complex hybrid query that uses most of the parser's capabilities.
//! It finds friends of "Alice" as of a specific time, ranks them by similarity to a query vector,
//! filters by age, and returns the top results.
//!
//! ```rust
//! use aletheiadb::query::parser::Parser;
//!
//! let query = "
//!     AS OF '2024-01-15T10:00:00Z'
//!     MATCH (alice:Person {name: 'Alice'})-[:KNOWS]->(friend)
//!     RANK BY SIMILARITY TO $query_embedding TOP 10
//!     WHERE friend.age > 25
//!     RETURN friend.name, friend.age
//!     ORDER BY friend.age DESC
//!     LIMIT 5
//! ";
//!
//! let ast = Parser::parse(query).unwrap();
//! assert!(ast.is_temporal());
//! assert!(ast.has_vector_ops());
//! ```
//!
//! ## Implementation Details
//!
//! The parser is implemented as a recursive descent parser. It consumes a stream of `Token`s
//! produced by the `Lexer`.
//!
//! - **Recursion Depth**: Limited to 100 to prevent stack overflow on deeply nested predicates.
//! - **Error Handling**: Returns detailed `ParseError`s with position information to help users debug syntax errors.

use std::sync::Arc;

use crate::index::vector::DistanceMetric;

use super::ast::*;
use super::lexer::{Lexer, LexerError, Token};

/// Maximum recursion depth for parsing expressions to prevent stack overflow.
const MAX_RECURSION_DEPTH: usize = 100;

/// Default result limit for vector similarity searches (SIMILAR TO, FIND SIMILAR).
const DEFAULT_VECTOR_SEARCH_LIMIT: usize = 10;

/// Maximum depth for unbounded variable-length traversals (`*n..`).
/// Uses half of `usize::MAX` to avoid overflow in range arithmetic.
const UNBOUNDED_MAX_DEPTH: usize = usize::MAX / 2;

/// Error type for parser errors.
///
/// This error provides detailed information about what went wrong during parsing,
/// including the position in the token stream and what was expected vs found.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::query::parser::Parser;
///
/// // Invalid syntax triggers a ParseError
/// let result = Parser::parse("MATCH n RETURN n");
/// assert!(result.is_err());
/// let error = result.unwrap_err();
/// assert!(error.message.contains("Expected ("));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// A descriptive error message explaining the failure.
    pub message: String,
    /// The index in the token stream where the error occurred.
    pub position: usize,
    /// A description of what token or construct was expected (optional).
    pub expected: Option<String>,
    /// The actual token found at the error position (optional).
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

/// A parser for the AQL query language.
///
/// The `Parser` maintains state (tokens and current position) as it walks through
/// the input stream. It is designed to be used via the static [`Parser::parse`] method.
pub struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    /// Parse a AQL query string into an Abstract Syntax Tree (AST).
    ///
    /// This is the main entry point for the parser. It tokenizes the input string
    /// and processes it according to the AQL grammar.
    ///
    /// # Arguments
    ///
    /// * `input` - The AQL query string to parse.
    ///
    /// # Returns
    ///
    /// * `Ok(QueryAst)` - The parsed AST if the query is valid.
    /// * `Err(ParseError)` - An error describing why parsing failed.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::query::parser::Parser;
    ///
    /// let query = "MATCH (n:Person) RETURN n.name";
    /// match Parser::parse(query) {
    ///     Ok(ast) => println!("Successfully parsed query: {:?}", ast),
    ///     Err(e) => eprintln!("Parse error: {}", e),
    /// }
    /// ```
    pub fn parse(input: &str) -> Result<QueryAst, ParseError> {
        let tokens = Lexer::tokenize(input)?;
        let mut parser = Parser {
            tokens,
            position: 0,
        };
        parser.parse_query()
    }

    /// Parse a complete AQL query.
    ///
    /// This is the top-level parsing function. It expects the query to follow the general structure:
    /// `[Temporal] [Source] [Rank] [Where] [Return] [Order] [Skip] [Limit]`
    ///
    /// # Grammar
    /// ```text
    /// query ::= temporal_clause?
    ///           source_clause
    ///           rank_clause?
    ///           where_clause?
    ///           return_clause?
    ///           order_clause?
    ///           skip_clause?
    ///           limit_clause?
    /// ```
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

    /// Parse an optional temporal clause (`AS OF ...` or `BETWEEN ...`).
    ///
    /// The temporal clause sets the context for the query (valid time and transaction time).
    ///
    /// # Grammar
    /// ```text
    /// temporal_clause ::= "AS OF" timestamp ("," timestamp)?
    ///                   | "BETWEEN" timestamp "AND" timestamp
    /// ```
    ///
    /// # Examples
    /// - `AS OF '2024-01-01'` (Valid time only)
    /// - `AS OF '2024-01-01', '2024-01-02'` (Valid time + Transaction time)
    /// - `BETWEEN '2024-01-01' AND '2024-02-01'` (Valid time range)
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

    /// Parse the main source of data for the query.
    ///
    /// This parses the `MATCH` clause or vector search clauses (`SIMILAR TO`, `FIND SIMILAR`).
    /// Every query must have exactly one source clause.
    ///
    /// # Grammar
    /// ```text
    /// source_clause ::= match_clause
    ///                 | similar_clause
    ///                 | find_similar_clause
    /// ```
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

    /// Parse a `MATCH` clause containing one or more patterns.
    ///
    /// The `MATCH` clause defines the graph patterns to search for. Multiple patterns
    /// can be comma-separated (e.g., `MATCH (a), (b)`).
    ///
    /// # Grammar
    /// ```text
    /// match_clause ::= "MATCH" pattern ("," pattern)*
    /// ```
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
            DEFAULT_VECTOR_SEARCH_LIMIT
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
            DEFAULT_VECTOR_SEARCH_LIMIT
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
        let mut values = Vec::with_capacity(128); // ⚡ Bolt Optimization: Pre-allocate capacity for typical vector embeddings to avoid O(log N) heap reallocations during parsing of large arrays.

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

    /// Parse a graph pattern consisting of nodes connected by relationships.
    ///
    /// Grammar:
    /// ```text
    /// pattern ::= node_pattern (relationship_pattern node_pattern)*
    /// ```
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
        let direction_start = self.parse_relationship_direction_start()?;

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
            if matches!(
                self.peek(),
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

        rel.direction = self.resolve_relationship_direction(direction_start);

        Ok(rel)
    }

    fn parse_relationship_direction_start(&mut self) -> Result<RelationshipDirection, ParseError> {
        if self.check(&Token::LeftArrow) {
            self.advance();
            Ok(RelationshipDirection::Incoming)
        } else {
            // Must be dash because is_relationship_start() checked it
            self.expect(&Token::Dash)?;
            Ok(RelationshipDirection::Both) // Default, may change to Outgoing
        }
    }

    fn resolve_relationship_direction(
        &mut self,
        start_dir: RelationshipDirection,
    ) -> RelationshipDirection {
        // Parse end arrow to determine final direction
        // Pattern combinations:
        //   -[]->  = Outgoing
        //   <-[]-  = Incoming
        //   -[]-   = Both
        //   <-[]-> = Both (bidirectional)
        if self.check(&Token::Arrow) {
            self.advance();
            if start_dir == RelationshipDirection::Incoming {
                // <-[]-> is bidirectional
                RelationshipDirection::Both
            } else {
                // -[]-> is outgoing
                RelationshipDirection::Outgoing
            }
        } else if self.check(&Token::Dash) {
            self.advance();
            if start_dir == RelationshipDirection::Incoming {
                // <-[]- stays as Incoming
                RelationshipDirection::Incoming
            } else {
                // -[]- is both
                RelationshipDirection::Both
            }
        } else {
            // If no ending marker, keep the start direction
            start_dir
        }
    }

    fn parse_depth_spec(&mut self) -> Result<DepthSpec, ParseError> {
        self.expect(&Token::Star)?;

        // Check for range or exact
        match self.current() {
            Some(Token::IntegerLiteral(n)) => {
                let min = self.validate_non_negative(*n)?;
                self.advance();

                if self.check(&Token::Dot) {
                    self.parse_depth_range(min)
                } else {
                    Ok(DepthSpec::Exact(min))
                }
            }
            Some(Token::Dot) => {
                self.advance();
                self.expect(&Token::Dot)?;

                if let Some(Token::IntegerLiteral(n)) = self.current() {
                    let max = self.validate_non_negative(*n)?;
                    self.advance();
                    Ok(DepthSpec::Max(max))
                } else {
                    Ok(DepthSpec::Variable)
                }
            }
            _ => Ok(DepthSpec::Variable),
        }
    }

    fn validate_non_negative(&self, n: i64) -> Result<usize, ParseError> {
        if n < 0 {
            Err(self.error(
                format!("Depth must be non-negative, got {}", n),
                Some("non-negative integer".to_string()),
            ))
        } else {
            Ok(n as usize)
        }
    }

    fn parse_depth_range(&mut self, min: usize) -> Result<DepthSpec, ParseError> {
        self.advance(); // consume first dot
        self.expect(&Token::Dot)?; // expect second dot

        if let Some(Token::IntegerLiteral(m)) = self.current() {
            let max = self.validate_non_negative(*m)?;
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
                max: UNBOUNDED_MAX_DEPTH,
            })
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
        self.parse_value().map_err(|e| {
            // Maintain original error message for backward compatibility/tests
            if e.message == "Expected value" {
                self.error(
                    "Expected property value".to_string(),
                    Some("value".to_string()),
                )
            } else {
                e
            }
        })
    }

    fn parse_value(&mut self) -> Result<PropertyValue, ParseError> {
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
            Some(Token::Dash) => self.parse_negative_number(),
            _ => Err(self.error("Expected value".to_string(), Some("value".to_string()))),
        }
    }

    fn parse_negative_number(&mut self) -> Result<PropertyValue, ParseError> {
        self.advance(); // consume Dash
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

    // =========================================================
    // RANK BY SIMILARITY Clause
    // =========================================================

    /// Parse a `RANK BY SIMILARITY` clause.
    ///
    /// This is an extension to AQL for hybrid search, allowing graph results to be
    /// re-ranked based on vector similarity.
    ///
    /// Grammar:
    /// ```text
    /// rank_clause ::= "RANK BY SIMILARITY TO" embedding ("TOP" integer)?
    /// ```
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

    /// Parse a `WHERE` clause containing predicates.
    ///
    /// The `WHERE` clause filters the results from the source clause using boolean logic.
    ///
    /// # Grammar
    /// ```text
    /// where_clause ::= "WHERE" predicate
    /// ```
    fn parse_where_clause(&mut self) -> Result<Option<WhereClause>, ParseError> {
        if !self.check(&Token::Where) {
            return Ok(None);
        }

        self.advance(); // WHERE
        let predicate = self.parse_predicate(0)?;

        Ok(Some(WhereClause { predicate }))
    }

    /// Parse a boolean predicate expression.
    ///
    /// This is the entry point for parsing conditions in `WHERE` clauses.
    /// It delegates to `parse_or_predicate` to handle operator precedence (OR has lowest precedence).
    ///
    /// # Recursion Limit
    ///
    /// Takes a `depth` argument to enforce `MAX_RECURSION_DEPTH` (100) to prevent stack overflows.
    fn parse_predicate(&mut self, depth: usize) -> Result<PredicateExpr, ParseError> {
        self.parse_or_predicate(depth)
    }

    /// Parse logical OR expressions.
    ///
    /// ```text
    /// or_predicate ::= and_predicate ("OR" and_predicate)*
    /// ```
    fn parse_or_predicate(&mut self, depth: usize) -> Result<PredicateExpr, ParseError> {
        let mut left = self.parse_and_predicate(depth)?;

        while self.check(&Token::Or) {
            self.advance();
            let right = self.parse_and_predicate(depth)?;
            left = PredicateExpr::Or(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    fn parse_and_predicate(&mut self, depth: usize) -> Result<PredicateExpr, ParseError> {
        let mut left = self.parse_not_predicate(depth)?;

        while self.check(&Token::And) {
            self.advance();
            let right = self.parse_not_predicate(depth)?;
            left = PredicateExpr::And(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    fn parse_not_predicate(&mut self, depth: usize) -> Result<PredicateExpr, ParseError> {
        self.check_recursion_depth(depth)?;

        if self.check(&Token::Not) {
            self.advance();
            let pred = self.parse_not_predicate(depth + 1)?;
            return Ok(PredicateExpr::Not(Box::new(pred)));
        }

        self.parse_primary_predicate(depth)
    }

    fn parse_primary_predicate(&mut self, depth: usize) -> Result<PredicateExpr, ParseError> {
        // Parenthesized predicate
        if self.check(&Token::LeftParen) {
            return self.parse_grouped_predicate(depth);
        }

        // EXISTS(n.prop)
        if self.check(&Token::Exists) {
            return self.parse_exists_predicate();
        }

        // Property-based predicates
        let expr = self.parse_expression()?;

        // IS [NOT] NULL
        if self.check(&Token::Is) {
            return self.parse_is_null_predicate(expr);
        }

        // CONTAINS, STARTS WITH, ENDS WITH
        if self.check(&Token::Contains) || self.check(&Token::Starts) || self.check(&Token::Ends) {
            return self.parse_string_predicate(expr);
        }

        // IN [list]
        if self.check(&Token::In) {
            return self.parse_in_predicate(expr);
        }

        // Comparison operators
        self.parse_comparison_predicate(expr)
    }

    fn parse_grouped_predicate(&mut self, depth: usize) -> Result<PredicateExpr, ParseError> {
        self.check_recursion_depth(depth)?;

        self.advance();
        let pred = self.parse_predicate(depth + 1)?;
        self.expect(&Token::RightParen)?;
        Ok(PredicateExpr::Grouped(Box::new(pred)))
    }

    fn parse_exists_predicate(&mut self) -> Result<PredicateExpr, ParseError> {
        self.advance();
        self.expect(&Token::LeftParen)?;
        let prop = self.parse_property_access()?;
        self.expect(&Token::RightParen)?;
        Ok(PredicateExpr::Exists(prop))
    }

    fn parse_is_null_predicate(&mut self, expr: Expression) -> Result<PredicateExpr, ParseError> {
        self.advance();
        let is_not = if self.check(&Token::Not) {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&Token::Null)?;

        let prop = self.require_property_expr(expr, "IS NULL")?;
        Ok(if is_not {
            PredicateExpr::IsNotNull(prop)
        } else {
            PredicateExpr::IsNull(prop)
        })
    }

    fn parse_string_predicate(&mut self, expr: Expression) -> Result<PredicateExpr, ParseError> {
        if self.check(&Token::Contains) {
            self.advance();
            let substring = self.parse_string()?;
            let property = self.require_property_expr(expr, "CONTAINS")?;
            return Ok(PredicateExpr::Contains {
                property,
                substring,
            });
        }

        if self.check(&Token::Starts) {
            self.advance();
            self.expect(&Token::With)?;
            let prefix = self.parse_string()?;
            let property = self.require_property_expr(expr, "STARTS WITH")?;
            return Ok(PredicateExpr::StartsWith { property, prefix });
        }

        if self.check(&Token::Ends) {
            self.advance();
            self.expect(&Token::With)?;
            let suffix = self.parse_string()?;
            let property = self.require_property_expr(expr, "ENDS WITH")?;
            return Ok(PredicateExpr::EndsWith { property, suffix });
        }

        Err(self.error("Expected string predicate".to_string(), None))
    }

    fn parse_in_predicate(&mut self, expr: Expression) -> Result<PredicateExpr, ParseError> {
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
        let property = self.require_property_expr(expr, "IN")?;
        Ok(PredicateExpr::In { property, values })
    }

    fn parse_comparison_predicate(
        &mut self,
        expr: Expression,
    ) -> Result<PredicateExpr, ParseError> {
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
            _ => self.parse_literal_expression(),
        }
    }

    fn parse_literal_expression(&mut self) -> Result<Expression, ParseError> {
        // Try to parse as value (literal or parameter)
        // Note: parse_expression handles parameters explicitly, so we shouldn't see them here,
        // but parse_value supports them.
        match self.parse_value() {
            Ok(val) => Ok(Expression::Literal(val)),
            Err(e) => {
                // Remap "Expected value" to "Expected expression" for context
                if e.message == "Expected value" {
                    Err(self.error(
                        "Expected expression".to_string(),
                        Some("identifier, literal, or parameter".to_string()),
                    ))
                } else {
                    Err(e)
                }
            }
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

    /// Parse a `RETURN` clause.
    ///
    /// The `RETURN` clause specifies which data to include in the result set.
    /// It supports aliasing (`AS alias`) and `DISTINCT` projections.
    ///
    /// # Grammar
    /// ```text
    /// return_clause ::= "RETURN" ("DISTINCT")? return_item ("," return_item)*
    /// return_item   ::= expression ("AS" identifier)?
    ///                 | "COUNT" "(" ("*" | expression) ")"
    /// ```
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

    /// Parse an `ORDER BY` clause.
    ///
    /// # Grammar
    /// ```text
    /// order_clause ::= "ORDER BY" order_item ("," order_item)*
    /// order_item   ::= expression ("ASC" | "DESC")?
    /// ```
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

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position + 1)
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

    fn check_recursion_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(self.error(
                format!("Recursion limit exceeded (max {MAX_RECURSION_DEPTH})"),
                None,
            ));
        }
        Ok(())
    }

    /// Extracts a [`PropertyAccess`] from an expression, returning a parse error
    /// if the expression is not a property reference. Used by predicates that
    /// require a property on the left-hand side (IS NULL, CONTAINS, IN, etc.).
    fn require_property_expr(
        &self,
        expr: Expression,
        context: &str,
    ) -> Result<PropertyAccess, ParseError> {
        if let Expression::Property(prop) = expr {
            Ok(prop)
        } else {
            Err(self.error(
                format!("{context} requires a property expression"),
                Some(format!("property.name {context} ...")),
            ))
        }
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
#[cfg(test)]
mod tests;
