//! Cypher Query Language Recursive Descent Parser
//!
//! Transforms a token stream (produced by [`CypherLexer`]) into an AST
//! ([`CypherStatement`]). The parser is a classic LL(1) recursive descent
//! parser -- each grammar rule maps to a method, and the current token
//! determines which production to use.
//!
//! # Grammar (simplified)
//!
//! ```text
//! statement    := [temporal] match_stmt
//! match_stmt   := [OPTIONAL] MATCH pattern_list [where_clause] [temporal] with_clauses return_clause
//! with_clauses := (WITH return_items [WHERE expr])*
//! pattern_list := pattern (',' pattern)*
//! pattern      := node_pattern (rel_pattern node_pattern)*
//! node_pattern := '(' [var] [':' label]* ['{' props '}'] ')'
//! rel_pattern  := '-' '[' ... ']' '->' | '<-' '[' ... ']' '-' | '-' '[' ... ']' '-'
//! where_clause := WHERE expr
//! return_clause:= RETURN [DISTINCT] return_items [order_by] [SKIP n] [LIMIT n]
//! expr         := or_expr
//! or_expr      := and_expr (OR and_expr)*
//! and_expr     := not_expr (AND not_expr)*
//! not_expr     := NOT not_expr | comparison
//! comparison   := primary (comp_op primary)?
//! primary      := value | var '.' prop | '(' expr ')' | func_call | var
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use aletheiadb::cypher::CypherParser;
//!
//! let ast = CypherParser::parse("MATCH (n:Person) RETURN n")?;
//! ```

use super::ast::*;
use super::error::CypherError;
use super::lexer::{CypherLexer, Token, TokenKind};

/// Maximum nesting depth allowed while parsing an expression.
///
/// The expression grammar recurses at three points: a parenthesised
/// sub-expression (`'(' expr ')'`), a `NOT` chain (`NOT not_expr`), and the
/// element expressions of an `IN [ ... ]` / function-argument list. Each of
/// these threads and increments a `depth` counter; when it exceeds this bound
/// the parser returns a [`CypherError::ParseError`] instead of recursing
/// further. Without the cap, a pathological input such as thousands of nested
/// parentheses or `NOT`s overflows the stack and aborts the whole process
/// (issue #3404). Legitimate queries nest far below this limit.
///
/// Note: the bound is *cumulative* across all nesting constructs
/// (parentheses, `NOT`, `IN [...]` element lists, and function arguments), not
/// a separate budget per construct -- e.g. deeply nested parens inside an
/// `IN`-list share the same counter.
const MAX_EXPRESSION_DEPTH: usize = 128;

/// Maximum number of operands in a single contiguous `AND` / `OR` chain.
///
/// The parser builds a left-nested `Box<CypherExpr>` spine for a flat
/// `a AND b AND c ...` (or `OR`) chain, which is intentionally *not* bounded by
/// [`MAX_EXPRESSION_DEPTH`] (the chain has no per-operand nesting). That spine
/// is walked iteratively at conversion time, but the derived recursive `Drop`
/// for `CypherExpr` still unwinds it one stack frame per node when the AST is
/// dropped -- so an unbounded chain (tens of thousands of terms) would
/// stack-overflow on teardown, relocating the very SIGABRT issue #3404 targets
/// to destruction. Capping the operand count keeps that drop depth bounded
/// while staying far beyond any realistic query; it doubles as an in-lane
/// query-complexity guard.
const MAX_EXPRESSION_TERMS: usize = 4096;

/// Recursive descent parser for Cypher queries.
///
/// Created internally by [`CypherParser::parse`]; callers never need to
/// construct one directly.
pub struct CypherParser {
    /// The complete token stream (always ends with `Eof`).
    tokens: Vec<Token>,
    /// Current position in the token stream.
    pos: usize,
}

impl CypherParser {
    /// Parse a Cypher query string into an AST.
    ///
    /// This is the main entry point. It lexes the input and then runs the
    /// recursive descent parser over the resulting token stream.
    ///
    /// # Errors
    ///
    /// Returns [`CypherError::LexError`] if tokenization fails, or
    /// [`CypherError::ParseError`] if the token stream does not form a valid
    /// Cypher statement.
    pub fn parse(input: &str) -> Result<CypherStatement, CypherError> {
        let tokens = CypherLexer::tokenize(input)?;
        let mut parser = Self { tokens, pos: 0 };
        let stmt = parser.parse_statement()?;

        // Ensure we consumed everything (except the trailing Eof).
        if parser.peek().kind != TokenKind::Eof {
            return Err(parser.error("unexpected tokens after end of statement"));
        }
        Ok(stmt)
    }

    // =======================================================================
    // Utility helpers
    // =======================================================================

    /// Return a reference to the current token without advancing.
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Return a reference to the current token and advance by one.
    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        if tok.kind != TokenKind::Eof {
            self.pos += 1;
        }
        tok
    }

    /// Assert that the current token is `kind`, consume it, and return it.
    ///
    /// # Errors
    ///
    /// Returns a parse error if the current token does not match.
    fn expect(&mut self, kind: TokenKind) -> Result<Token, CypherError> {
        if self.peek().kind == kind {
            Ok(self.advance().clone())
        } else {
            Err(self.error(&format!("expected {kind:?}, found {:?}", self.peek().kind)))
        }
    }

    /// Check whether the current token is `kind` without consuming it.
    fn at(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    /// If the current token is `kind`, consume it and return `true`.
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Build a [`CypherError::ParseError`] at the current token position.
    fn error(&self, message: &str) -> CypherError {
        CypherError::ParseError {
            position: self.peek().position,
            message: message.to_string(),
        }
    }

    // =======================================================================
    // Grammar rules
    // =======================================================================

    /// ```text
    /// statement := [temporal] match_stmt
    /// ```
    fn parse_statement(&mut self) -> Result<CypherStatement, CypherError> {
        // Check for leading temporal clause (AS OF / FOR / BETWEEN).
        let temporal = self.try_parse_temporal()?;

        // A standalone `UNWIND ... AS ... RETURN ...` statement (Issue #559).
        // A leading temporal clause has no meaning for a listless UNWIND, so
        // reject the combination rather than silently ignoring the qualifier.
        if self.at(TokenKind::Unwind) {
            if temporal.is_some() {
                return Err(self.error(
                    "a temporal clause (AS OF / FOR / BETWEEN) cannot precede a standalone UNWIND",
                ));
            }
            return self.parse_unwind();
        }

        // Now expect a MATCH (or OPTIONAL MATCH).
        let stmt = self.parse_match(temporal)?;
        Ok(stmt)
    }

    /// Parse a standalone `UNWIND <list> AS <var> RETURN ...` statement.
    ///
    /// ```text
    /// unwind_stmt := UNWIND expr AS identifier return_clause
    /// ```
    ///
    /// The list source is parsed as a full expression (so a list literal
    /// `[...]`, a parameter `$list`, or the `null` literal are all accepted and
    /// depth-capped through [`Self::parse_expression`]); whether the resolved
    /// value is actually list-shaped is validated at execution time.
    fn parse_unwind(&mut self) -> Result<CypherStatement, CypherError> {
        self.expect(TokenKind::Unwind)?;
        let source = self.parse_expression(0)?;
        self.expect(TokenKind::As)?;
        let var_tok = self.expect(TokenKind::Identifier)?;
        let return_clause = self.parse_return()?;
        Ok(CypherStatement::Unwind {
            source,
            variable: var_tok.text,
            return_clause,
        })
    }

    /// ```text
    /// match_stmt := [OPTIONAL] MATCH pattern_list [where_clause] [temporal_clause]
    ///               (with_clause | optional_match_clause)* return_clause
    /// optional_match_clause := OPTIONAL MATCH pattern_list [where_clause]
    /// ```
    fn parse_match(
        &mut self,
        temporal: Option<CypherTemporal>,
    ) -> Result<CypherStatement, CypherError> {
        let optional = self.eat(TokenKind::OptionalMatch);
        self.expect(TokenKind::Match)?;

        let pattern = self.parse_pattern_list()?;

        let where_clause = if self.at(TokenKind::Where) {
            Some(self.parse_where()?)
        } else {
            None
        };

        // Check for post-pattern temporal clause (between WHERE and RETURN).
        // If a leading temporal clause was already parsed, this is skipped.
        let temporal = if temporal.is_some() {
            temporal
        } else {
            self.try_parse_post_pattern_temporal()?
        };

        // Parse zero or more WITH and OPTIONAL MATCH clauses (in any order)
        // between pattern/WHERE/temporal and RETURN. Source ordering between
        // WITH clauses and OPTIONAL MATCH clauses is preserved via
        // `preceding_withs` on each `CypherOptionalMatch`.
        let mut with_clauses = Vec::new();
        let mut optional_matches = Vec::new();
        loop {
            if self.at(TokenKind::With) {
                with_clauses.push(self.parse_with()?);
            } else if self.at(TokenKind::OptionalMatch) {
                optional_matches.push(self.parse_optional_match(with_clauses.len())?);
            } else {
                break;
            }
        }

        let return_clause = self.parse_return()?;

        Ok(CypherStatement::Match {
            optional,
            pattern,
            where_clause,
            return_clause,
            temporal,
            with_clauses,
            optional_matches,
        })
    }

    /// Parse a subsequent `OPTIONAL MATCH` clause.
    ///
    /// ```text
    /// optional_match_clause := OPTIONAL MATCH pattern_list [where_clause]
    /// ```
    fn parse_optional_match(
        &mut self,
        preceding_withs: usize,
    ) -> Result<CypherOptionalMatch, CypherError> {
        self.expect(TokenKind::OptionalMatch)?;
        self.expect(TokenKind::Match)?;

        let pattern = self.parse_pattern_list()?;

        let where_clause = if self.at(TokenKind::Where) {
            Some(self.parse_where()?)
        } else {
            None
        };

        Ok(CypherOptionalMatch {
            pattern,
            where_clause,
            preceding_withs,
        })
    }

    // -- Patterns -----------------------------------------------------------

    /// ```text
    /// pattern_list := pattern (',' pattern)*
    /// ```
    fn parse_pattern_list(&mut self) -> Result<Vec<CypherPattern>, CypherError> {
        let mut patterns = vec![self.parse_pattern()?];
        while self.eat(TokenKind::Comma) {
            patterns.push(self.parse_pattern()?);
        }
        Ok(patterns)
    }

    /// ```text
    /// pattern := node_pattern (rel_pattern node_pattern)*
    /// ```
    fn parse_pattern(&mut self) -> Result<CypherPattern, CypherError> {
        let mut elements = Vec::new();
        elements.push(CypherPatternElement::Node(self.parse_node_pattern()?));

        // While we see the start of a relationship (`-` or `<-`), keep parsing
        // relationship + node pairs.
        while self.at(TokenKind::Dash) || self.at(TokenKind::LeftArrow) {
            elements.push(CypherPatternElement::Relationship(
                self.parse_relationship_pattern()?,
            ));
            elements.push(CypherPatternElement::Node(self.parse_node_pattern()?));
        }

        Ok(CypherPattern { elements })
    }

    /// ```text
    /// node_pattern := '(' [var] [':' label]* ['{' props '}'] ')'
    /// ```
    fn parse_node_pattern(&mut self) -> Result<CypherNodePattern, CypherError> {
        self.expect(TokenKind::LParen)?;

        let variable = if self.at(TokenKind::Identifier) {
            let tok = self.advance().clone();
            Some(tok.text)
        } else {
            None
        };

        let mut labels = Vec::new();
        while self.eat(TokenKind::Colon) {
            let label_tok = self.expect(TokenKind::Identifier)?;
            labels.push(label_tok.text);
        }

        let properties = if self.at(TokenKind::LBrace) {
            self.parse_properties()?
        } else {
            Vec::new()
        };

        self.expect(TokenKind::RParen)?;

        Ok(CypherNodePattern {
            variable,
            labels,
            properties,
        })
    }

    /// Parse a relationship pattern. Handles three direction forms:
    ///
    /// - Outgoing: `-[...]->`
    /// - Incoming: `<-[...]- `
    /// - Both: `-[...]-`
    fn parse_relationship_pattern(&mut self) -> Result<CypherRelPattern, CypherError> {
        // Determine direction prefix.
        let incoming = self.eat(TokenKind::LeftArrow); // `<-`

        if !incoming {
            // Must start with `-`
            self.expect(TokenKind::Dash)?;
        }

        // Parse the bracket contents: `[var :TYPE|TYPE *depth {props}]`
        self.expect(TokenKind::LBracket)?;

        let variable = if self.at(TokenKind::Identifier) {
            let tok = self.advance().clone();
            Some(tok.text)
        } else {
            None
        };

        let mut rel_types = Vec::new();
        if self.eat(TokenKind::Colon) {
            let type_tok = self.expect(TokenKind::Identifier)?;
            rel_types.push(type_tok.text);
            while self.eat(TokenKind::Pipe) {
                let type_tok = self.expect(TokenKind::Identifier)?;
                rel_types.push(type_tok.text);
            }
        }

        let depth = if self.eat(TokenKind::Star) {
            Some(self.parse_depth()?)
        } else {
            None
        };

        let properties = if self.at(TokenKind::LBrace) {
            self.parse_properties()?
        } else {
            Vec::new()
        };

        self.expect(TokenKind::RBracket)?;

        // Determine direction suffix.
        let direction = if incoming {
            // We already consumed `<-` ... `]`, now expect trailing `-`.
            self.expect(TokenKind::Dash)?;
            CypherDirection::Incoming
        } else if self.eat(TokenKind::Arrow) {
            // `-[...]->`
            CypherDirection::Outgoing
        } else {
            // `-[...]-`
            self.expect(TokenKind::Dash)?;
            CypherDirection::Both
        };

        Ok(CypherRelPattern {
            variable,
            rel_types,
            direction,
            depth,
            properties,
        })
    }

    /// Parse a variable-length depth specifier (everything after `*`).
    ///
    /// ```text
    /// depth := ε | N | N '..' M | '..' M | N '..'
    /// ```
    fn parse_depth(&mut self) -> Result<CypherDepth, CypherError> {
        // After `*`, we may see:
        //   nothing        => Unbounded
        //   N              => Exact(N) or start of Range
        //   ..M            => Max(M)
        //   N..            => Min(N)
        //   N..M           => Range{min, max}

        if self.at(TokenKind::DotDot) {
            // `*..M`
            self.advance();
            let max = self.parse_usize("expected integer after '..'")?;
            return Ok(CypherDepth::Max(max));
        }

        if self.at(TokenKind::IntegerLiteral) {
            let n = self.parse_usize("expected integer for depth")?;
            if self.eat(TokenKind::DotDot) {
                // `*N..` or `*N..M`
                if self.at(TokenKind::IntegerLiteral) {
                    let m = self.parse_usize("expected integer after '..'")?;
                    if n > m {
                        return Err(
                            self.error(&format!("invalid depth range: min ({n}) > max ({m})"))
                        );
                    }
                    Ok(CypherDepth::Range { min: n, max: m })
                } else {
                    Ok(CypherDepth::Min(n))
                }
            } else {
                Ok(CypherDepth::Exact(n))
            }
        } else {
            // Bare `*` with no number or `..` following.
            Ok(CypherDepth::Unbounded)
        }
    }

    /// Parse `{key: value, ...}` property map.
    fn parse_properties(&mut self) -> Result<Vec<(String, CypherValue)>, CypherError> {
        self.expect(TokenKind::LBrace)?;
        let mut props = Vec::new();

        if !self.at(TokenKind::RBrace) {
            loop {
                let key = self.expect(TokenKind::Identifier)?;
                self.expect(TokenKind::Colon)?;
                let value = self.parse_value()?;
                props.push((key.text, value));
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
        }

        self.expect(TokenKind::RBrace)?;
        Ok(props)
    }

    // -- WHERE / expressions ------------------------------------------------

    /// ```text
    /// where_clause := WHERE expr
    /// ```
    fn parse_where(&mut self) -> Result<CypherExpr, CypherError> {
        self.expect(TokenKind::Where)?;
        self.parse_expression(0)
    }

    /// Entry point for expression parsing.
    ///
    /// `depth` is the current expression nesting level; it is incremented only
    /// at genuine recursion re-entry points (parenthesised sub-expression,
    /// `NOT`, and `IN`/argument element lists) and passed through unchanged by
    /// the operator-precedence layers. Exceeding [`MAX_EXPRESSION_DEPTH`]
    /// returns a [`CypherError::ParseError`] rather than overflowing the stack.
    ///
    /// ```text
    /// expr := or_expr
    /// ```
    fn parse_expression(&mut self, depth: usize) -> Result<CypherExpr, CypherError> {
        if depth > MAX_EXPRESSION_DEPTH {
            return Err(self.error("expression nesting too deep (max 128)"));
        }
        self.parse_or_expr(depth)
    }

    /// ```text
    /// or_expr := and_expr (OR and_expr)*
    /// ```
    fn parse_or_expr(&mut self, depth: usize) -> Result<CypherExpr, CypherError> {
        let mut left = self.parse_and_expr(depth)?;
        // Operand count for this contiguous OR chain (starts at 1 for `left`).
        let mut terms: usize = 1;
        while self.eat(TokenKind::Or) {
            terms += 1;
            if terms > MAX_EXPRESSION_TERMS {
                return Err(self.error("expression has too many AND/OR terms (max 4096)"));
            }
            let right = self.parse_and_expr(depth)?;
            left = CypherExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// ```text
    /// and_expr := not_expr (AND not_expr)*
    /// ```
    fn parse_and_expr(&mut self, depth: usize) -> Result<CypherExpr, CypherError> {
        let mut left = self.parse_not_expr(depth)?;
        // Operand count for this contiguous AND chain (starts at 1 for `left`).
        let mut terms: usize = 1;
        while self.eat(TokenKind::And) {
            terms += 1;
            if terms > MAX_EXPRESSION_TERMS {
                return Err(self.error("expression has too many AND/OR terms (max 4096)"));
            }
            let right = self.parse_not_expr(depth)?;
            left = CypherExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// ```text
    /// not_expr := NOT not_expr | comparison
    /// ```
    fn parse_not_expr(&mut self, depth: usize) -> Result<CypherExpr, CypherError> {
        if depth > MAX_EXPRESSION_DEPTH {
            return Err(self.error("expression nesting too deep (max 128)"));
        }
        if self.eat(TokenKind::Not) {
            let inner = self.parse_not_expr(depth + 1)?;
            Ok(CypherExpr::Not(Box::new(inner)))
        } else {
            self.parse_comparison(depth)
        }
    }

    /// ```text
    /// comparison := primary (comp_op primary | IS [NOT] NULL | IN '[' expr_list ']'
    ///            | CONTAINS string | STARTS WITH string | ENDS WITH string)?
    /// ```
    fn parse_comparison(&mut self, depth: usize) -> Result<CypherExpr, CypherError> {
        let left = self.parse_primary_expr(depth)?;

        // Standard comparison operators
        let op = match self.peek().kind {
            TokenKind::Eq => Some(CypherCompOp::Eq),
            TokenKind::Ne => Some(CypherCompOp::Ne),
            TokenKind::Lt => Some(CypherCompOp::Lt),
            TokenKind::Le => Some(CypherCompOp::Le),
            TokenKind::Gt => Some(CypherCompOp::Gt),
            TokenKind::Ge => Some(CypherCompOp::Ge),
            _ => None,
        };

        if let Some(op) = op {
            self.advance();
            let right = self.parse_primary_expr(depth)?;
            return Ok(CypherExpr::Comparison {
                left: Box::new(left),
                op,
                right: Box::new(right),
            });
        }

        // IS NULL / IS NOT NULL
        if self.at(TokenKind::Is) {
            self.advance();
            if self.eat(TokenKind::Not) {
                self.expect(TokenKind::Null)?;
                return Ok(CypherExpr::IsNotNull(Box::new(left)));
            }
            self.expect(TokenKind::Null)?;
            return Ok(CypherExpr::IsNull(Box::new(left)));
        }

        // IN [values]
        if self.at(TokenKind::In) {
            self.advance();
            self.expect(TokenKind::LBracket)?;
            let mut values = vec![];
            if !self.at(TokenKind::RBracket) {
                values.push(self.parse_expression(depth + 1)?);
                while self.eat(TokenKind::Comma) {
                    values.push(self.parse_expression(depth + 1)?);
                }
            }
            self.expect(TokenKind::RBracket)?;
            return Ok(CypherExpr::In {
                expr: Box::new(left),
                values,
            });
        }

        // CONTAINS string
        if self.at(TokenKind::Contains) {
            self.advance();
            let right = self.parse_primary_expr(depth)?;
            if let CypherExpr::Value(CypherValue::String(s)) = right {
                return Ok(CypherExpr::Contains {
                    expr: Box::new(left),
                    substring: s,
                });
            }
            return Err(self.error("CONTAINS requires a string argument"));
        }

        // STARTS WITH string
        if self.at(TokenKind::StartsWith) {
            self.advance(); // consume STARTS
            self.expect(TokenKind::With)?; // consume WITH
            let right = self.parse_primary_expr(depth)?;
            if let CypherExpr::Value(CypherValue::String(s)) = right {
                return Ok(CypherExpr::StartsWith {
                    expr: Box::new(left),
                    prefix: s,
                });
            }
            return Err(self.error("STARTS WITH requires a string argument"));
        }

        // ENDS WITH string
        if self.at(TokenKind::EndsWith) {
            self.advance(); // consume ENDS
            self.expect(TokenKind::With)?; // consume WITH
            let right = self.parse_primary_expr(depth)?;
            if let CypherExpr::Value(CypherValue::String(s)) = right {
                return Ok(CypherExpr::EndsWith {
                    expr: Box::new(left),
                    suffix: s,
                });
            }
            return Err(self.error("ENDS WITH requires a string argument"));
        }

        Ok(left)
    }

    /// ```text
    /// primary := value | var '.' prop | '(' expr ')' | func_call | var
    /// ```
    fn parse_primary_expr(&mut self, depth: usize) -> Result<CypherExpr, CypherError> {
        match self.peek().kind.clone() {
            // Parenthesized sub-expression
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_expression(depth + 1)?;
                self.expect(TokenKind::RParen)?;
                Ok(CypherExpr::Grouped(Box::new(inner)))
            }

            // List literal: `[ expr (',' expr)* ]` (empty list allowed).
            //
            // Each element is parsed one nesting level deeper so that a
            // pathologically nested list literal (`[[[ ... ]]]`) is bounded by
            // MAX_EXPRESSION_DEPTH and returns a parse error instead of
            // overflowing the stack (issue #3404).
            TokenKind::LBracket => {
                self.advance();
                let mut elements = Vec::new();
                if !self.at(TokenKind::RBracket) {
                    elements.push(self.parse_expression(depth + 1)?);
                    while self.eat(TokenKind::Comma) {
                        // Cap the element count (mirroring MAX_EXPRESSION_TERMS
                        // for AND/OR chains): a pathological multi-MB literal
                        // would otherwise allocate ~1M AST nodes. Return a
                        // graceful ParseError instead (issue #3404-adjacent).
                        if elements.len() >= MAX_EXPRESSION_TERMS {
                            return Err(self.error("list literal has too many elements (max 4096)"));
                        }
                        elements.push(self.parse_expression(depth + 1)?);
                    }
                }
                self.expect(TokenKind::RBracket)?;
                Ok(CypherExpr::List(elements))
            }

            // Identifier: could be variable, property access, dot-qualified function call,
            // or simple function call
            TokenKind::Identifier => {
                let name_tok = self.advance().clone();
                let name = name_tok.text;

                if self.at(TokenKind::Dot) {
                    // Could be property access: var.prop
                    // Or dot-qualified function call: namespace.func(args...)
                    self.advance(); // consume '.'
                    let next_tok = self.expect(TokenKind::Identifier)?;

                    if self.at(TokenKind::LParen) {
                        // Dot-qualified function call: namespace.func(args...)
                        // e.g., vector.similarity(d.embedding, $query)
                        self.advance(); // consume '('
                        let args = self.parse_function_args(depth)?;
                        self.expect(TokenKind::RParen)?;
                        let qualified_name = format!("{name}.{}", next_tok.text);
                        Ok(CypherExpr::FunctionCall {
                            name: qualified_name,
                            args,
                            distinct: false,
                        })
                    } else {
                        // Property access: var.prop
                        Ok(CypherExpr::Property {
                            variable: name,
                            property: next_tok.text,
                        })
                    }
                } else if self.at(TokenKind::LParen) {
                    // Function call: name(args...)
                    self.advance();
                    let args = self.parse_function_args(depth)?;
                    self.expect(TokenKind::RParen)?;
                    Ok(CypherExpr::FunctionCall {
                        name,
                        args,
                        distinct: false,
                    })
                } else {
                    // Bare variable
                    Ok(CypherExpr::Variable(name))
                }
            }

            // Aggregation functions (COUNT, AVG, etc.) that are keywords, not identifiers
            TokenKind::Count
            | TokenKind::Collect
            | TokenKind::Avg
            | TokenKind::Sum
            | TokenKind::Min
            | TokenKind::Max => {
                let name_tok = self.advance().clone();
                let name = name_tok.text.to_uppercase();
                self.expect(TokenKind::LParen)?;
                // Optional DISTINCT quantifier: count(DISTINCT n.dept).
                let distinct = self.eat(TokenKind::Distinct);
                // `count(*)` -- the star wildcard is only valid as the sole
                // argument (typically for count). `DISTINCT *` is rejected
                // (openCypher does not allow `count(DISTINCT *)`).
                let args = if self.at(TokenKind::Star) {
                    if distinct {
                        return Err(self.error(
                            "DISTINCT * is not allowed (use count(*) or count(DISTINCT expr))",
                        ));
                    }
                    self.advance();
                    vec![CypherExpr::Star]
                } else {
                    self.parse_function_args(depth)?
                };
                self.expect(TokenKind::RParen)?;
                Ok(CypherExpr::FunctionCall {
                    name,
                    args,
                    distinct,
                })
            }

            // Literal values
            TokenKind::IntegerLiteral
            | TokenKind::FloatLiteral
            | TokenKind::StringLiteral
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Null
            | TokenKind::Parameter => {
                let val = self.parse_value()?;
                Ok(CypherExpr::Value(val))
            }

            _ => Err(self.error(&format!(
                "expected expression, found {:?}",
                self.peek().kind
            ))),
        }
    }

    /// Parse comma-separated function arguments (may be empty).
    ///
    /// Each argument is a fresh expression nested one level below the call, so
    /// `depth` is incremented to bound deeply nested `f(g(h(...)))` chains.
    fn parse_function_args(&mut self, depth: usize) -> Result<Vec<CypherExpr>, CypherError> {
        let mut args = Vec::new();
        if !self.at(TokenKind::RParen) {
            args.push(self.parse_expression(depth + 1)?);
            while self.eat(TokenKind::Comma) {
                args.push(self.parse_expression(depth + 1)?);
            }
        }
        Ok(args)
    }

    // -- WITH clause --------------------------------------------------------

    /// Parse a single `WITH` clause.
    ///
    /// ```text
    /// with_clause := WITH return_items [WHERE expr]
    /// ```
    fn parse_with(&mut self) -> Result<CypherWith, CypherError> {
        self.expect(TokenKind::With)?;

        let items = self.parse_return_items()?;

        let where_clause = if self.at(TokenKind::Where) {
            Some(self.parse_where()?)
        } else {
            None
        };

        Ok(CypherWith {
            items,
            where_clause,
        })
    }

    // -- RETURN clause ------------------------------------------------------

    /// ```text
    /// return_clause := RETURN [DISTINCT] return_items [order_by] [SKIP n] [LIMIT n]
    /// ```
    fn parse_return(&mut self) -> Result<CypherReturn, CypherError> {
        self.expect(TokenKind::Return)?;

        let distinct = self.eat(TokenKind::Distinct);

        let items = self.parse_return_items()?;

        let order_by = if self.at(TokenKind::Order) {
            self.parse_order_by()?
        } else {
            Vec::new()
        };

        let skip = if self.eat(TokenKind::Skip) {
            Some(self.parse_usize("expected integer after SKIP")?)
        } else {
            None
        };

        let limit = if self.eat(TokenKind::Limit) {
            Some(self.parse_usize("expected integer after LIMIT")?)
        } else {
            None
        };

        Ok(CypherReturn {
            distinct,
            items,
            order_by,
            skip,
            limit,
        })
    }

    /// ```text
    /// return_items := '*' | return_item (',' return_item)*
    /// ```
    fn parse_return_items(&mut self) -> Result<Vec<CypherReturnItem>, CypherError> {
        if self.eat(TokenKind::Star) {
            return Ok(vec![CypherReturnItem::Star]);
        }

        let mut items = vec![self.parse_return_item()?];
        while self.eat(TokenKind::Comma) {
            items.push(self.parse_return_item()?);
        }
        Ok(items)
    }

    /// ```text
    /// return_item := expr [AS identifier]
    /// ```
    fn parse_return_item(&mut self) -> Result<CypherReturnItem, CypherError> {
        let expr = self.parse_expression(0)?;

        let alias = if self.eat(TokenKind::As) {
            let alias_tok = self.expect(TokenKind::Identifier)?;
            Some(alias_tok.text)
        } else {
            None
        };

        Ok(CypherReturnItem::Expression { expr, alias })
    }

    /// ```text
    /// order_by := ORDER BY order_item (',' order_item)*
    /// ```
    fn parse_order_by(&mut self) -> Result<Vec<CypherOrderItem>, CypherError> {
        self.expect(TokenKind::Order)?;
        self.expect(TokenKind::By)?;

        let mut items = vec![self.parse_order_item()?];
        while self.eat(TokenKind::Comma) {
            items.push(self.parse_order_item()?);
        }
        Ok(items)
    }

    /// ```text
    /// order_item := expr [ASC | DESC]
    /// ```
    fn parse_order_item(&mut self) -> Result<CypherOrderItem, CypherError> {
        let expr = self.parse_expression(0)?;

        let descending = if self.eat(TokenKind::Desc) {
            true
        } else {
            self.eat(TokenKind::Asc);
            false
        };

        Ok(CypherOrderItem { expr, descending })
    }

    // -- Values -------------------------------------------------------------

    /// Parse a single literal value or parameter.
    fn parse_value(&mut self) -> Result<CypherValue, CypherError> {
        match self.peek().kind.clone() {
            TokenKind::IntegerLiteral => {
                let tok = self.advance().clone();
                let n: i64 = tok
                    .text
                    .parse()
                    .map_err(|_| self.error("invalid integer literal"))?;
                Ok(CypherValue::Int(n))
            }
            TokenKind::FloatLiteral => {
                let tok = self.advance().clone();
                let f: f64 = tok
                    .text
                    .parse()
                    .map_err(|_| self.error("invalid float literal"))?;
                Ok(CypherValue::Float(f))
            }
            TokenKind::StringLiteral => {
                let tok = self.advance().clone();
                Ok(CypherValue::String(tok.text))
            }
            TokenKind::True => {
                self.advance();
                Ok(CypherValue::Bool(true))
            }
            TokenKind::False => {
                self.advance();
                Ok(CypherValue::Bool(false))
            }
            TokenKind::Null => {
                self.advance();
                Ok(CypherValue::Null)
            }
            TokenKind::Parameter => {
                let tok = self.advance().clone();
                Ok(CypherValue::Parameter(tok.text))
            }
            _ => Err(self.error(&format!("expected value, found {:?}", self.peek().kind))),
        }
    }

    // -- Temporal -----------------------------------------------------------

    /// Try to parse a leading temporal clause. Returns `None` if the next token
    /// is not the start of a temporal clause.
    ///
    /// Supported forms:
    /// - `AS OF TIMESTAMP '...'`
    /// - `FOR VALID_TIME AS OF '...'`
    /// - `FOR SYSTEM_TIME AS OF '...'`
    /// - `BETWEEN '...' AND '...'`
    fn try_parse_temporal(&mut self) -> Result<Option<CypherTemporal>, CypherError> {
        match self.peek().kind {
            TokenKind::As => {
                // AS OF TIMESTAMP '...'
                self.advance(); // AS
                self.expect(TokenKind::Of)?;
                self.expect(TokenKind::Timestamp)?;
                let ts_tok = self.expect(TokenKind::StringLiteral)?;
                Ok(Some(CypherTemporal::AsOfTimestamp(ts_tok.text)))
            }
            TokenKind::For => {
                // FOR VALID_TIME/SYSTEM_TIME AS OF '...'
                self.advance(); // FOR
                match self.peek().kind {
                    TokenKind::ValidTime => {
                        self.advance();
                        self.expect(TokenKind::As)?;
                        self.expect(TokenKind::Of)?;
                        let ts_tok = self.expect(TokenKind::StringLiteral)?;
                        Ok(Some(CypherTemporal::AsOfValidTime(ts_tok.text)))
                    }
                    TokenKind::SystemTime => {
                        self.advance();
                        self.expect(TokenKind::As)?;
                        self.expect(TokenKind::Of)?;
                        let ts_tok = self.expect(TokenKind::StringLiteral)?;
                        Ok(Some(CypherTemporal::AsOfSystemTime(ts_tok.text)))
                    }
                    _ => Err(self.error("expected VALID_TIME or SYSTEM_TIME after FOR")),
                }
            }
            TokenKind::Between => {
                // BETWEEN '...' AND '...'
                self.advance(); // BETWEEN
                let start_tok = self.expect(TokenKind::StringLiteral)?;
                self.expect(TokenKind::And)?;
                let end_tok = self.expect(TokenKind::StringLiteral)?;
                Ok(Some(CypherTemporal::Between {
                    start: start_tok.text,
                    end: end_tok.text,
                }))
            }
            _ => Ok(None),
        }
    }

    // -- Post-pattern temporal -----------------------------------------------

    /// Try to parse a temporal clause that appears after the pattern/WHERE
    /// but before RETURN. Returns `None` if the next token is not the start
    /// of a temporal clause.
    ///
    /// Supported forms (post-pattern position):
    /// - `AS OF TIMESTAMP '...'`
    /// - `AS OF VALID_TIME '...'`
    /// - `AS OF SYSTEM_TIME '...'`
    /// - `AS OF VALID_TIME '...' AS OF SYSTEM_TIME '...'`
    /// - `FOR SYSTEM_TIME AS OF '...'`
    /// - `BETWEEN '...' AND '...'`
    fn try_parse_post_pattern_temporal(&mut self) -> Result<Option<CypherTemporal>, CypherError> {
        match self.peek().kind {
            TokenKind::As => {
                self.advance(); // AS
                self.expect(TokenKind::Of)?;

                match self.peek().kind {
                    TokenKind::Timestamp => {
                        // AS OF TIMESTAMP '...'
                        self.advance();
                        let ts_tok = self.expect(TokenKind::StringLiteral)?;
                        Ok(Some(CypherTemporal::AsOfTimestamp(ts_tok.text)))
                    }
                    TokenKind::ValidTime => {
                        // AS OF VALID_TIME '...' [AS OF SYSTEM_TIME '...']
                        self.advance();
                        let vt_tok = self.expect(TokenKind::StringLiteral)?;

                        // Check for bi-temporal: AS OF SYSTEM_TIME '...'
                        if self.at(TokenKind::As) {
                            let saved_pos = self.pos;
                            self.advance(); // AS
                            if self.at(TokenKind::Of) {
                                self.advance(); // OF
                                if self.at(TokenKind::SystemTime) {
                                    self.advance(); // SYSTEM_TIME
                                    let st_tok = self.expect(TokenKind::StringLiteral)?;
                                    return Ok(Some(CypherTemporal::BiTemporal {
                                        valid_time: vt_tok.text,
                                        system_time: st_tok.text,
                                    }));
                                }
                                // Not SYSTEM_TIME -- backtrack
                                self.pos = saved_pos;
                            } else {
                                // Not OF -- backtrack
                                self.pos = saved_pos;
                            }
                        }

                        Ok(Some(CypherTemporal::AsOfValidTime(vt_tok.text)))
                    }
                    TokenKind::SystemTime => {
                        // AS OF SYSTEM_TIME '...'
                        self.advance();
                        let ts_tok = self.expect(TokenKind::StringLiteral)?;
                        Ok(Some(CypherTemporal::AsOfSystemTime(ts_tok.text)))
                    }
                    _ => {
                        Err(self
                            .error("expected TIMESTAMP, VALID_TIME, or SYSTEM_TIME after AS OF"))
                    }
                }
            }
            TokenKind::For => {
                self.advance(); // FOR
                if self.at(TokenKind::SystemTime) {
                    // FOR SYSTEM_TIME AS OF '...'
                    self.advance(); // SYSTEM_TIME
                    self.expect(TokenKind::As)?;
                    self.expect(TokenKind::Of)?;
                    let ts_tok = self.expect(TokenKind::StringLiteral)?;
                    Ok(Some(CypherTemporal::AsOfSystemTime(ts_tok.text)))
                } else if self.at(TokenKind::ValidTime) {
                    // FOR VALID_TIME AS OF '...'
                    self.advance(); // VALID_TIME
                    self.expect(TokenKind::As)?;
                    self.expect(TokenKind::Of)?;
                    let ts_tok = self.expect(TokenKind::StringLiteral)?;
                    Ok(Some(CypherTemporal::AsOfValidTime(ts_tok.text)))
                } else {
                    Err(self.error("expected SYSTEM_TIME or VALID_TIME after FOR"))
                }
            }
            TokenKind::Between => {
                self.advance(); // BETWEEN
                let start_tok = self.expect(TokenKind::StringLiteral)?;
                self.expect(TokenKind::And)?;
                let end_tok = self.expect(TokenKind::StringLiteral)?;
                Ok(Some(CypherTemporal::Between {
                    start: start_tok.text,
                    end: end_tok.text,
                }))
            }
            _ => Ok(None),
        }
    }

    // -- Numeric helpers ----------------------------------------------------

    /// Parse the current token as a `usize`.
    fn parse_usize(&mut self, msg: &str) -> Result<usize, CypherError> {
        let tok = self.expect(TokenKind::IntegerLiteral)?;
        tok.text
            .parse::<usize>()
            .map_err(|_| self.error(&format!("{msg}: '{}'", tok.text)))
    }
}
