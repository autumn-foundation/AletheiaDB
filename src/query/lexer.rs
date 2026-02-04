//! Query Language Lexer
//!
//! This module provides the `Lexer` which transforms a raw GQL (Gallifrey Query Language)
//! query string into a stream of structured [`Token`]s. This is the first stage of the
//! query compilation pipeline.
//!
//! # Overview
//!
//! The lexer handles:
//! - **Keywords**: Graph (`MATCH`), Vector (`SIMILAR TO`), Temporal (`AS OF`), etc.
//! - **Literals**: Strings, integers, floats.
//! - **Identifiers**: Variable names, labels, property keys.
//! - **Operators & Punctuation**: Arrows (`->`), comparison (`=`, `<`), delimiters.
//! - **Parameters**: Query parameters starting with `$` (e.g., `$name`).
//!
//! # Example
//!
//! ```rust
//! use gallifreydb::query::lexer::{Lexer, Token};
//!
//! let input = "MATCH (n:Person) RETURN n";
//! let tokens = Lexer::tokenize(input).unwrap();
//!
//! assert_eq!(tokens[0], Token::Match);
//! assert_eq!(tokens[1], Token::LeftParen);
//! assert_eq!(tokens[2], Token::Identifier("n".to_string()));
//! assert_eq!(tokens[3], Token::Colon);
//! assert_eq!(tokens[4], Token::Identifier("Person".to_string()));
//! assert_eq!(tokens[5], Token::RightParen);
//! assert_eq!(tokens[6], Token::Return);
//! assert_eq!(tokens[7], Token::Identifier("n".to_string()));
//! assert_eq!(tokens[8], Token::Eof);
//! ```

use std::fmt;

/// A token in the GQL (Gallifrey Query Language) stream.
///
/// Tokens represent the smallest meaningful units of the query language, such as keywords,
/// literals, identifiers, and punctuation.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // ========================================================================
    // Keywords - Graph Pattern Matching
    // ========================================================================

    /// The `MATCH` keyword, used to specify graph patterns.
    Match,
    /// The `WHERE` keyword, used to filter results based on predicates.
    Where,
    /// The `RETURN` keyword, used to specify what to return from the query.
    Return,
    /// The `ORDER` keyword, used in `ORDER BY` clauses.
    Order,
    /// The `BY` keyword, used in `ORDER BY` and `RANK BY` clauses.
    By,
    /// The `LIMIT` keyword, used to restrict the number of results.
    Limit,
    /// The `SKIP` keyword, used to skip a number of results for pagination.
    Skip,
    /// The `DISTINCT` keyword, used to filter duplicate results.
    Distinct,
    /// The `COUNT` keyword, used for aggregation.
    Count,
    /// The `ASC` keyword, specifying ascending sort order.
    Asc,
    /// The `DESC` keyword, specifying descending sort order.
    Desc,

    // ========================================================================
    // Keywords - Logical & Predicates
    // ========================================================================

    /// The `AND` logical operator.
    And,
    /// The `OR` logical operator.
    Or,
    /// The `NOT` logical operator.
    Not,
    /// The `IN` operator, checking if a value exists in a list.
    In,
    /// The `IS` operator, used in `IS NULL`.
    Is,
    /// The `NULL` literal value.
    Null,
    /// The `TRUE` boolean literal.
    True,
    /// The `FALSE` boolean literal.
    False,

    // ========================================================================
    // Keywords - Vector Search
    // ========================================================================

    /// The `SIMILAR` keyword, used in vector similarity searches.
    Similar,
    /// The `TO` keyword, used in `SIMILAR TO`.
    To,
    /// The `USING` keyword, used to specify a distance metric.
    Using,
    /// The `FIND` keyword, used in `FIND SIMILAR`.
    Find,
    /// The `RANK` keyword, used in `RANK BY SIMILARITY` hybrid queries.
    Rank,
    /// The `SIMILARITY` keyword, used in `RANK BY SIMILARITY`.
    Similarity,
    /// The `TOP` keyword, used to limit vector search candidates (e.g., `TOP 10`).
    Top,

    // ========================================================================
    // Keywords - Temporal
    // ========================================================================

    /// The `AS` keyword, used in `AS OF` temporal clauses or for aliasing (`RETURN n AS name`).
    As,
    /// The `OF` keyword, used in `AS OF`.
    Of,
    /// The `BETWEEN` keyword, used for temporal range queries.
    Between,

    // ========================================================================
    // Keywords - String Predicates
    // ========================================================================

    /// The `EXISTS` predicate function.
    Exists,
    /// The `CONTAINS` string predicate.
    Contains,
    /// The `STARTS` keyword, used in `STARTS WITH`.
    Starts,
    /// The `ENDS` keyword, used in `ENDS WITH`.
    Ends,
    /// The `WITH` keyword, used in `STARTS WITH` and `ENDS WITH`.
    With,

    // ========================================================================
    // Keywords - Distance Metrics
    // ========================================================================

    /// The `COSINE` distance metric.
    Cosine,
    /// The `EUCLIDEAN` distance metric.
    Euclidean,
    /// The `DOT_PRODUCT` distance metric.
    DotProduct,

    // ========================================================================
    // Punctuation & Delimiters
    // ========================================================================

    /// Left parenthesis `(`.
    LeftParen,
    /// Right parenthesis `)`.
    RightParen,
    /// Left bracket `[` (for lists or relationship types).
    LeftBracket,
    /// Right bracket `]` (for lists or relationship types).
    RightBracket,
    /// Left brace `{` (for property maps).
    LeftBrace,
    /// Right brace `}` (for property maps).
    RightBrace,
    /// Colon `:`, used for labels and property keys.
    Colon,
    /// Comma `,`, used as a separator.
    Comma,
    /// Dot `.`, used for property access.
    Dot,
    /// Asterisk `*`, used for variable length paths or multiplication (reserved).
    Star,
    /// Dash `-`, used in relationship patterns.
    Dash,

    // ========================================================================
    // Arrow Patterns
    // ========================================================================

    /// Right arrow `->` for outgoing relationships.
    Arrow,
    /// Left arrow `<-` for incoming relationships.
    LeftArrow,

    // ========================================================================
    // Comparison Operators
    // ========================================================================

    /// Equality operator `=`.
    Eq,
    /// Inequality operator `<>` or `!=`.
    Ne,
    /// Less than operator `<`.
    Lt,
    /// Less than or equal operator `<=`.
    Le,
    /// Greater than operator `>`.
    Gt,
    /// Greater than or equal operator `>=`.
    Ge,

    // ========================================================================
    // Literals & Identifiers
    // ========================================================================

    /// An identifier (variable name, label, property key).
    Identifier(String),
    /// A string literal (e.g., `'hello'`).
    StringLiteral(String),
    /// An integer literal (e.g., `42`).
    IntegerLiteral(i64),
    /// A floating-point literal (e.g., `3.14`).
    FloatLiteral(f64),
    /// A query parameter (e.g., `$param`).
    Parameter(String),

    // ========================================================================
    // Special
    // ========================================================================

    /// End of file/input marker.
    Eof,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Match => write!(f, "MATCH"),
            Token::Where => write!(f, "WHERE"),
            Token::Return => write!(f, "RETURN"),
            Token::Order => write!(f, "ORDER"),
            Token::By => write!(f, "BY"),
            Token::Limit => write!(f, "LIMIT"),
            Token::Skip => write!(f, "SKIP"),
            Token::Distinct => write!(f, "DISTINCT"),
            Token::Count => write!(f, "COUNT"),
            Token::Asc => write!(f, "ASC"),
            Token::Desc => write!(f, "DESC"),
            Token::And => write!(f, "AND"),
            Token::Or => write!(f, "OR"),
            Token::Not => write!(f, "NOT"),
            Token::In => write!(f, "IN"),
            Token::Is => write!(f, "IS"),
            Token::Null => write!(f, "NULL"),
            Token::True => write!(f, "TRUE"),
            Token::False => write!(f, "FALSE"),
            Token::Similar => write!(f, "SIMILAR"),
            Token::To => write!(f, "TO"),
            Token::Using => write!(f, "USING"),
            Token::Find => write!(f, "FIND"),
            Token::Rank => write!(f, "RANK"),
            Token::Similarity => write!(f, "SIMILARITY"),
            Token::Top => write!(f, "TOP"),
            Token::As => write!(f, "AS"),
            Token::Of => write!(f, "OF"),
            Token::Between => write!(f, "BETWEEN"),
            Token::Exists => write!(f, "EXISTS"),
            Token::Contains => write!(f, "CONTAINS"),
            Token::Starts => write!(f, "STARTS"),
            Token::Ends => write!(f, "ENDS"),
            Token::With => write!(f, "WITH"),
            Token::Cosine => write!(f, "COSINE"),
            Token::Euclidean => write!(f, "EUCLIDEAN"),
            Token::DotProduct => write!(f, "DOT_PRODUCT"),
            Token::LeftParen => write!(f, "("),
            Token::RightParen => write!(f, ")"),
            Token::LeftBracket => write!(f, "["),
            Token::RightBracket => write!(f, "]"),
            Token::LeftBrace => write!(f, "{{"),
            Token::RightBrace => write!(f, "}}"),
            Token::Colon => write!(f, ":"),
            Token::Comma => write!(f, ","),
            Token::Dot => write!(f, "."),
            Token::Star => write!(f, "*"),
            Token::Dash => write!(f, "-"),
            Token::Arrow => write!(f, "->"),
            Token::LeftArrow => write!(f, "<-"),
            Token::Eq => write!(f, "="),
            Token::Ne => write!(f, "<>"),
            Token::Lt => write!(f, "<"),
            Token::Le => write!(f, "<="),
            Token::Gt => write!(f, ">"),
            Token::Ge => write!(f, ">="),
            Token::Identifier(s) => write!(f, "{}", s),
            Token::StringLiteral(s) => write!(f, "'{}'", s),
            Token::IntegerLiteral(n) => write!(f, "{}", n),
            Token::FloatLiteral(n) => write!(f, "{}", n),
            Token::Parameter(s) => write!(f, "${}", s),
            Token::Eof => write!(f, "EOF"),
        }
    }
}

/// Error type for lexer errors.
///
/// Contains details about the error location (line, column, byte position) to help
/// with error reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct LexerError {
    /// Error message describing what went wrong.
    pub message: String,
    /// Byte position in the input where the error occurred.
    pub position: usize,
    /// Line number (1-indexed) where the error occurred.
    pub line: usize,
    /// Column number (1-indexed) where the error occurred.
    pub column: usize,
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Lexer error at line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for LexerError {}

/// A lexer for the GQL query language.
///
/// Maintains state (current position, line, column) while iterating through the input string.
/// It is usually easier to use the static [`Lexer::tokenize`] method rather than instantiating
/// `Lexer` directly.
pub struct Lexer<'a> {
    input: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    position: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    /// Create a new lexer for the given input.
    ///
    /// # Arguments
    ///
    /// * `input` - The raw GQL query string to tokenize.
    pub fn new(input: &'a str) -> Self {
        Lexer {
            input,
            chars: input.char_indices().peekable(),
            position: 0,
            line: 1,
            column: 1,
        }
    }

    /// Tokenize the entire input and return a vector of tokens.
    ///
    /// This is the main entry point for using the lexer.
    ///
    /// # Arguments
    ///
    /// * `input` - The raw GQL query string.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Token>)` - A vector of tokens ending with `Token::Eof`.
    /// * `Err(LexerError)` - If an invalid character or sequence is encountered.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::query::lexer::{Lexer, Token};
    ///
    /// let tokens = Lexer::tokenize("RETURN 42").unwrap();
    /// assert_eq!(tokens, vec![Token::Return, Token::IntegerLiteral(42), Token::Eof]);
    /// ```
    pub fn tokenize(input: &str) -> Result<Vec<Token>, LexerError> {
        let mut lexer = Lexer::new(input);
        let mut tokens = Vec::new();

        loop {
            let token = lexer.next_token()?;
            let is_eof = token == Token::Eof;
            tokens.push(token);
            if is_eof {
                break;
            }
        }

        Ok(tokens)
    }

    /// Get the next token from the input.
    ///
    /// Advances the lexer state and returns the next token found. Skips whitespace
    /// and comments automatically.
    ///
    /// # Returns
    ///
    /// * `Ok(Token)` - The next token.
    /// * `Err(LexerError)` - If an error occurs.
    pub fn next_token(&mut self) -> Result<Token, LexerError> {
        self.skip_whitespace_and_comments()?;

        let Some(&(pos, ch)) = self.chars.peek() else {
            return Ok(Token::Eof);
        };

        self.position = pos;

        match ch {
            // Single character tokens
            '(' => {
                self.advance();
                Ok(Token::LeftParen)
            }
            ')' => {
                self.advance();
                Ok(Token::RightParen)
            }
            '[' => {
                self.advance();
                Ok(Token::LeftBracket)
            }
            ']' => {
                self.advance();
                Ok(Token::RightBracket)
            }
            '{' => {
                self.advance();
                Ok(Token::LeftBrace)
            }
            '}' => {
                self.advance();
                Ok(Token::RightBrace)
            }
            ':' => {
                self.advance();
                Ok(Token::Colon)
            }
            ',' => {
                self.advance();
                Ok(Token::Comma)
            }
            '.' => {
                self.advance();
                // Note: We don't support floats starting with . (like .5)
                // Use 0.5 instead. This allows us to parse 1..3 as range syntax.
                Ok(Token::Dot)
            }
            '*' => {
                self.advance();
                Ok(Token::Star)
            }
            '=' => {
                self.advance();
                Ok(Token::Eq)
            }

            // Multi-character operators
            '-' => self.read_dash_or_arrow(),
            '<' => self.read_less_than(),
            '>' => self.read_greater_than(),
            '!' => self.read_not_equal(),

            // String literals
            '\'' | '"' => self.read_string(),

            // Parameter
            '$' => self.read_parameter(),

            // Number or negative number
            '0'..='9' => self.read_number(),

            // Identifier or keyword
            'a'..='z' | 'A'..='Z' | '_' => self.read_identifier_or_keyword(),

            _ => Err(self.error(format!("Unexpected character: '{}'", ch))),
        }
    }

    fn advance(&mut self) -> Option<(usize, char)> {
        let result = self.chars.next();
        if let Some((_, ch)) = result {
            if ch == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
        result
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), LexerError> {
        loop {
            // Skip whitespace
            while let Some(&(_, ch)) = self.chars.peek() {
                if ch.is_whitespace() {
                    self.advance();
                } else {
                    break;
                }
            }

            // Check for comments
            if let Some(&(_, ch)) = self.chars.peek() {
                if ch == '-' {
                    // Check for -- comment
                    let mut chars_clone = self.chars.clone();
                    chars_clone.next();
                    if let Some(&(_, '-')) = chars_clone.peek() {
                        // Skip to end of line
                        self.advance(); // first -
                        self.advance(); // second -
                        while let Some(&(_, ch)) = self.chars.peek() {
                            if ch == '\n' {
                                self.advance();
                                break;
                            }
                            self.advance();
                        }
                        continue;
                    }
                } else if ch == '/' {
                    // Check for // or /* comment
                    let mut chars_clone = self.chars.clone();
                    chars_clone.next();
                    if let Some(&(_, next_ch)) = chars_clone.peek() {
                        if next_ch == '/' {
                            // Skip to end of line
                            self.advance(); // first /
                            self.advance(); // second /
                            while let Some(&(_, ch)) = self.chars.peek() {
                                if ch == '\n' {
                                    self.advance();
                                    break;
                                }
                                self.advance();
                            }
                            continue;
                        } else if next_ch == '*' {
                            // Skip to */
                            self.advance(); // /
                            self.advance(); // *
                            let mut found_end = false;
                            loop {
                                match self.advance() {
                                    Some((_, '*')) => {
                                        if let Some(&(_, '/')) = self.chars.peek() {
                                            self.advance();
                                            found_end = true;
                                            break;
                                        }
                                    }
                                    None => break,
                                    _ => {}
                                }
                            }
                            if !found_end {
                                return Err(self.error("Unterminated block comment".to_string()));
                            }
                            continue;
                        }
                    }
                }
            }
            break;
        }
        Ok(())
    }

    fn read_dash_or_arrow(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '-'

        if let Some(&(_, ch)) = self.chars.peek() {
            match ch {
                '>' => {
                    self.advance();
                    return Ok(Token::Arrow);
                }
                '[' | '(' => {
                    // This is a relationship start, just return dash
                    return Ok(Token::Dash);
                }
                '0'..='9' => {
                    // Negative number
                    return self.read_negative_number();
                }
                _ => {}
            }
        }

        Ok(Token::Dash)
    }

    fn read_less_than(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '<'

        if let Some(&(_, ch)) = self.chars.peek() {
            match ch {
                '=' => {
                    self.advance();
                    return Ok(Token::Le);
                }
                '>' => {
                    self.advance();
                    return Ok(Token::Ne);
                }
                '-' => {
                    self.advance();
                    return Ok(Token::LeftArrow);
                }
                _ => {}
            }
        }

        Ok(Token::Lt)
    }

    fn read_greater_than(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '>'

        if let Some(&(_, '=')) = self.chars.peek() {
            self.advance();
            return Ok(Token::Ge);
        }

        Ok(Token::Gt)
    }

    fn read_not_equal(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '!'

        if let Some(&(_, '=')) = self.chars.peek() {
            self.advance();
            return Ok(Token::Ne);
        }

        Err(self.error("Expected '=' after '!'".to_string()))
    }

    fn read_string(&mut self) -> Result<Token, LexerError> {
        let quote = self
            .advance()
            .map(|(_, c)| c)
            .ok_or_else(|| self.error("Unexpected EOF while reading string".to_string()))?;
        let mut value = String::new();

        loop {
            match self.advance() {
                Some((_, ch)) if ch == quote => {
                    // Check for escaped quote
                    if let Some(&(_, next_ch)) = self.chars.peek()
                        && next_ch == quote
                    {
                        value.push(quote);
                        self.advance();
                        continue;
                    }
                    break;
                }
                Some((_, '\\')) => {
                    // Handle escape sequences
                    match self.advance() {
                        Some((_, 'n')) => value.push('\n'),
                        Some((_, 't')) => value.push('\t'),
                        Some((_, 'r')) => value.push('\r'),
                        Some((_, '\\')) => value.push('\\'),
                        Some((_, ch)) if ch == quote => value.push(quote),
                        Some((_, ch)) => {
                            value.push('\\');
                            value.push(ch);
                        }
                        None => return Err(self.error("Unterminated string".to_string())),
                    }
                }
                Some((_, ch)) => value.push(ch),
                None => return Err(self.error("Unterminated string".to_string())),
            }
        }

        Ok(Token::StringLiteral(value))
    }

    fn read_parameter(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '$'
        let mut name = String::new();

        while let Some(&(_, ch)) = self.chars.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                name.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        if name.is_empty() {
            return Err(self.error("Expected parameter name after '$'".to_string()));
        }

        Ok(Token::Parameter(name))
    }

    fn read_number(&mut self) -> Result<Token, LexerError> {
        let start_pos = self.position;
        let mut has_dot = false;
        let mut has_exp = false;

        while let Some(&(pos, ch)) = self.chars.peek() {
            match ch {
                '0'..='9' => {
                    self.advance();
                }
                '.' if !has_dot && !has_exp => {
                    // Check if next char is a digit
                    let mut chars_clone = self.chars.clone();
                    chars_clone.next();
                    if let Some(&(_, next_ch)) = chars_clone.peek() {
                        if next_ch.is_ascii_digit() {
                            has_dot = true;
                            self.advance();
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                'e' | 'E' if !has_exp => {
                    has_exp = true;
                    has_dot = true; // Exponent implies float
                    self.advance();
                    // Optional sign after exponent
                    if let Some(&(_, sign)) = self.chars.peek()
                        && (sign == '+' || sign == '-')
                    {
                        self.advance();
                    }
                }
                _ => break,
            }
            let _ = pos; // silence unused warning
        }

        let text = &self.input[start_pos..self.position + self.current_offset()];
        self.parse_number(text, has_dot)
    }

    fn read_negative_number(&mut self) -> Result<Token, LexerError> {
        // We already consumed the '-'
        // Update position to current location (the first digit) so read_number starts there
        if let Some(&(pos, _)) = self.chars.peek() {
            self.position = pos;
        }
        // Read the number and negate it
        let token = self.read_number()?;
        match token {
            Token::IntegerLiteral(n) => Ok(Token::IntegerLiteral(-n)),
            Token::FloatLiteral(f) => Ok(Token::FloatLiteral(-f)),
            _ => Err(self.error("Expected number after '-'".to_string())),
        }
    }

    fn current_offset(&self) -> usize {
        self.chars
            .clone()
            .next()
            .map(|(pos, _)| pos - self.position)
            .unwrap_or(self.input.len() - self.position)
    }

    fn parse_number(&self, text: &str, is_float: bool) -> Result<Token, LexerError> {
        if is_float {
            text.parse::<f64>()
                .map(Token::FloatLiteral)
                .map_err(|_| self.error(format!("Invalid float: {}", text)))
        } else {
            text.parse::<i64>()
                .map(Token::IntegerLiteral)
                .map_err(|_| self.error(format!("Invalid integer: {}", text)))
        }
    }

    fn read_identifier_or_keyword(&mut self) -> Result<Token, LexerError> {
        let start_pos = self.position;

        while let Some(&(_, ch)) = self.chars.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.advance();
            } else {
                break;
            }
        }

        let end_pos = self
            .chars
            .peek()
            .map(|(pos, _)| *pos)
            .unwrap_or(self.input.len());
        let text = &self.input[start_pos..end_pos];

        // Check for keywords (case-insensitive)
        let token = match text.to_uppercase().as_str() {
            "MATCH" => Token::Match,
            "WHERE" => Token::Where,
            "RETURN" => Token::Return,
            "ORDER" => Token::Order,
            "BY" => Token::By,
            "LIMIT" => Token::Limit,
            "SKIP" => Token::Skip,
            "DISTINCT" => Token::Distinct,
            "COUNT" => Token::Count,
            "ASC" => Token::Asc,
            "DESC" => Token::Desc,
            "AND" => Token::And,
            "OR" => Token::Or,
            "NOT" => Token::Not,
            "IN" => Token::In,
            "IS" => Token::Is,
            "NULL" => Token::Null,
            "TRUE" => Token::True,
            "FALSE" => Token::False,
            "SIMILAR" => Token::Similar,
            "TO" => Token::To,
            "USING" => Token::Using,
            "FIND" => Token::Find,
            "RANK" => Token::Rank,
            "SIMILARITY" => Token::Similarity,
            "TOP" => Token::Top,
            "AS" => Token::As,
            "OF" => Token::Of,
            "BETWEEN" => Token::Between,
            "EXISTS" => Token::Exists,
            "CONTAINS" => Token::Contains,
            "STARTS" => Token::Starts,
            "ENDS" => Token::Ends,
            "WITH" => Token::With,
            "COSINE" => Token::Cosine,
            "EUCLIDEAN" => Token::Euclidean,
            "DOT_PRODUCT" => Token::DotProduct,
            _ => Token::Identifier(text.to_string()),
        };

        Ok(token)
    }

    fn error(&self, message: String) -> LexerError {
        LexerError {
            message,
            position: self.position,
            line: self.line,
            column: self.column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =====================================================
    // Basic Token Tests
    // =====================================================

    #[test]
    fn test_empty_input() {
        let tokens = Lexer::tokenize("").unwrap();
        assert_eq!(tokens, vec![Token::Eof]);
    }

    #[test]
    fn test_whitespace_only() {
        let tokens = Lexer::tokenize("   \n\t  ").unwrap();
        assert_eq!(tokens, vec![Token::Eof]);
    }

    // =====================================================
    // Keyword Tests
    // =====================================================

    #[test]
    fn test_graph_keywords() {
        let tokens = Lexer::tokenize("MATCH WHERE RETURN ORDER BY LIMIT SKIP").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::Where,
                Token::Return,
                Token::Order,
                Token::By,
                Token::Limit,
                Token::Skip,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_keywords_case_insensitive() {
        let tokens = Lexer::tokenize("match Match MATCH").unwrap();
        assert_eq!(
            tokens,
            vec![Token::Match, Token::Match, Token::Match, Token::Eof]
        );
    }

    #[test]
    fn test_vector_keywords() {
        let tokens = Lexer::tokenize("SIMILAR TO USING FIND RANK SIMILARITY TOP").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Similar,
                Token::To,
                Token::Using,
                Token::Find,
                Token::Rank,
                Token::Similarity,
                Token::Top,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_temporal_keywords() {
        let tokens = Lexer::tokenize("AS OF BETWEEN").unwrap();
        assert_eq!(
            tokens,
            vec![Token::As, Token::Of, Token::Between, Token::Eof]
        );
    }

    #[test]
    fn test_logical_keywords() {
        let tokens = Lexer::tokenize("AND OR NOT IN IS NULL TRUE FALSE").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::And,
                Token::Or,
                Token::Not,
                Token::In,
                Token::Is,
                Token::Null,
                Token::True,
                Token::False,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_string_predicate_keywords() {
        let tokens = Lexer::tokenize("EXISTS CONTAINS STARTS ENDS WITH").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Exists,
                Token::Contains,
                Token::Starts,
                Token::Ends,
                Token::With,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_metric_keywords() {
        let tokens = Lexer::tokenize("COSINE EUCLIDEAN DOT_PRODUCT").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Cosine,
                Token::Euclidean,
                Token::DotProduct,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_result_keywords() {
        let tokens = Lexer::tokenize("DISTINCT COUNT ASC DESC").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Distinct,
                Token::Count,
                Token::Asc,
                Token::Desc,
                Token::Eof
            ]
        );
    }

    // =====================================================
    // Punctuation Tests
    // =====================================================

    #[test]
    fn test_punctuation() {
        let tokens = Lexer::tokenize("()[]{}:,.*").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::LeftParen,
                Token::RightParen,
                Token::LeftBracket,
                Token::RightBracket,
                Token::LeftBrace,
                Token::RightBrace,
                Token::Colon,
                Token::Comma,
                Token::Dot,
                Token::Star,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_arrows() {
        let tokens = Lexer::tokenize("-> <- -").unwrap();
        assert_eq!(
            tokens,
            vec![Token::Arrow, Token::LeftArrow, Token::Dash, Token::Eof]
        );
    }

    // =====================================================
    // Operator Tests
    // =====================================================

    #[test]
    fn test_comparison_operators() {
        let tokens = Lexer::tokenize("= <> != < <= > >=").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Eq,
                Token::Ne,
                Token::Ne,
                Token::Lt,
                Token::Le,
                Token::Gt,
                Token::Ge,
                Token::Eof
            ]
        );
    }

    // =====================================================
    // Identifier Tests
    // =====================================================

    #[test]
    fn test_identifiers() {
        let tokens = Lexer::tokenize("foo bar_baz _underscore camelCase").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Identifier("foo".to_string()),
                Token::Identifier("bar_baz".to_string()),
                Token::Identifier("_underscore".to_string()),
                Token::Identifier("camelCase".to_string()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_identifier_with_numbers() {
        let tokens = Lexer::tokenize("node1 var2name item_3").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Identifier("node1".to_string()),
                Token::Identifier("var2name".to_string()),
                Token::Identifier("item_3".to_string()),
                Token::Eof
            ]
        );
    }

    // =====================================================
    // String Literal Tests
    // =====================================================

    #[test]
    fn test_single_quoted_string() {
        let tokens = Lexer::tokenize("'hello world'").unwrap();
        assert_eq!(
            tokens,
            vec![Token::StringLiteral("hello world".to_string()), Token::Eof]
        );
    }

    #[test]
    fn test_double_quoted_string() {
        let tokens = Lexer::tokenize("\"hello world\"").unwrap();
        assert_eq!(
            tokens,
            vec![Token::StringLiteral("hello world".to_string()), Token::Eof]
        );
    }

    #[test]
    fn test_string_with_escape_sequences() {
        let tokens = Lexer::tokenize("'hello\\nworld\\ttab'").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::StringLiteral("hello\nworld\ttab".to_string()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_string_with_escaped_quote() {
        let tokens = Lexer::tokenize("'it''s escaped'").unwrap();
        assert_eq!(
            tokens,
            vec![Token::StringLiteral("it's escaped".to_string()), Token::Eof]
        );
    }

    #[test]
    fn test_empty_string() {
        let tokens = Lexer::tokenize("''").unwrap();
        assert_eq!(
            tokens,
            vec![Token::StringLiteral("".to_string()), Token::Eof]
        );
    }

    // =====================================================
    // Number Literal Tests
    // =====================================================

    #[test]
    fn test_integer_literals() {
        let tokens = Lexer::tokenize("0 42 12345").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::IntegerLiteral(0),
                Token::IntegerLiteral(42),
                Token::IntegerLiteral(12345),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_negative_integer() {
        let tokens = Lexer::tokenize("-42").unwrap();
        assert_eq!(tokens, vec![Token::IntegerLiteral(-42), Token::Eof]);
    }

    #[test]
    fn test_float_literals() {
        let tokens = Lexer::tokenize("2.71 0.5 10.0").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::FloatLiteral(2.71),
                Token::FloatLiteral(0.5),
                Token::FloatLiteral(10.0),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_negative_float() {
        let tokens = Lexer::tokenize("-2.71").unwrap();
        assert_eq!(tokens, vec![Token::FloatLiteral(-2.71), Token::Eof]);
    }

    #[test]
    fn test_scientific_notation() {
        let tokens = Lexer::tokenize("1e10 2.5E-3 1e+5").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::FloatLiteral(1e10),
                Token::FloatLiteral(2.5e-3),
                Token::FloatLiteral(1e5),
                Token::Eof
            ]
        );
    }

    // =====================================================
    // Parameter Tests
    // =====================================================

    #[test]
    fn test_parameters() {
        let tokens = Lexer::tokenize("$embedding $user_id $1").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Parameter("embedding".to_string()),
                Token::Parameter("user_id".to_string()),
                Token::Parameter("1".to_string()),
                Token::Eof
            ]
        );
    }

    // =====================================================
    // Comment Tests
    // =====================================================

    #[test]
    fn test_line_comment_double_dash() {
        let tokens = Lexer::tokenize("MATCH -- this is a comment\nWHERE").unwrap();
        assert_eq!(tokens, vec![Token::Match, Token::Where, Token::Eof]);
    }

    #[test]
    fn test_line_comment_double_slash() {
        let tokens = Lexer::tokenize("MATCH // this is a comment\nWHERE").unwrap();
        assert_eq!(tokens, vec![Token::Match, Token::Where, Token::Eof]);
    }

    #[test]
    fn test_block_comment() {
        let tokens = Lexer::tokenize("MATCH /* multi\nline\ncomment */ WHERE").unwrap();
        assert_eq!(tokens, vec![Token::Match, Token::Where, Token::Eof]);
    }

    // =====================================================
    // Complex Query Tests
    // =====================================================

    #[test]
    fn test_simple_match_query() {
        let tokens = Lexer::tokenize("MATCH (n:Person) RETURN n").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::LeftParen,
                Token::Identifier("n".to_string()),
                Token::Colon,
                Token::Identifier("Person".to_string()),
                Token::RightParen,
                Token::Return,
                Token::Identifier("n".to_string()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_match_with_properties() {
        let tokens = Lexer::tokenize("MATCH (n:Person {name: 'Alice'})").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::LeftParen,
                Token::Identifier("n".to_string()),
                Token::Colon,
                Token::Identifier("Person".to_string()),
                Token::LeftBrace,
                Token::Identifier("name".to_string()),
                Token::Colon,
                Token::StringLiteral("Alice".to_string()),
                Token::RightBrace,
                Token::RightParen,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_match_with_relationship() {
        let tokens = Lexer::tokenize("MATCH (a)-[:KNOWS]->(b)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::LeftParen,
                Token::Identifier("a".to_string()),
                Token::RightParen,
                Token::Dash,
                Token::LeftBracket,
                Token::Colon,
                Token::Identifier("KNOWS".to_string()),
                Token::RightBracket,
                Token::Arrow,
                Token::LeftParen,
                Token::Identifier("b".to_string()),
                Token::RightParen,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_vector_search_query() {
        let tokens = Lexer::tokenize("SIMILAR TO $embedding USING COSINE LIMIT 10").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Similar,
                Token::To,
                Token::Parameter("embedding".to_string()),
                Token::Using,
                Token::Cosine,
                Token::Limit,
                Token::IntegerLiteral(10),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_temporal_query() {
        let tokens = Lexer::tokenize("AS OF '2024-01-15' MATCH (n)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::As,
                Token::Of,
                Token::StringLiteral("2024-01-15".to_string()),
                Token::Match,
                Token::LeftParen,
                Token::Identifier("n".to_string()),
                Token::RightParen,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_where_clause() {
        let tokens = Lexer::tokenize("WHERE n.age > 18 AND n.name = 'Alice'").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Where,
                Token::Identifier("n".to_string()),
                Token::Dot,
                Token::Identifier("age".to_string()),
                Token::Gt,
                Token::IntegerLiteral(18),
                Token::And,
                Token::Identifier("n".to_string()),
                Token::Dot,
                Token::Identifier("name".to_string()),
                Token::Eq,
                Token::StringLiteral("Alice".to_string()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_hybrid_query() {
        let tokens = Lexer::tokenize(
            "AS OF '2024-01-01' MATCH (a:Person)-[:KNOWS]->(b) RANK BY SIMILARITY TO $embedding TOP 10",
        )
        .unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::As,
                Token::Of,
                Token::StringLiteral("2024-01-01".to_string()),
                Token::Match,
                Token::LeftParen,
                Token::Identifier("a".to_string()),
                Token::Colon,
                Token::Identifier("Person".to_string()),
                Token::RightParen,
                Token::Dash,
                Token::LeftBracket,
                Token::Colon,
                Token::Identifier("KNOWS".to_string()),
                Token::RightBracket,
                Token::Arrow,
                Token::LeftParen,
                Token::Identifier("b".to_string()),
                Token::RightParen,
                Token::Rank,
                Token::By,
                Token::Similarity,
                Token::To,
                Token::Parameter("embedding".to_string()),
                Token::Top,
                Token::IntegerLiteral(10),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_order_by_clause() {
        let tokens = Lexer::tokenize("ORDER BY n.age DESC, n.name ASC").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Order,
                Token::By,
                Token::Identifier("n".to_string()),
                Token::Dot,
                Token::Identifier("age".to_string()),
                Token::Desc,
                Token::Comma,
                Token::Identifier("n".to_string()),
                Token::Dot,
                Token::Identifier("name".to_string()),
                Token::Asc,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_variable_length_path() {
        let tokens = Lexer::tokenize("MATCH (a)-[:KNOWS*1..3]->(b)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::LeftParen,
                Token::Identifier("a".to_string()),
                Token::RightParen,
                Token::Dash,
                Token::LeftBracket,
                Token::Colon,
                Token::Identifier("KNOWS".to_string()),
                Token::Star,
                Token::IntegerLiteral(1),
                Token::Dot,
                Token::Dot,
                Token::IntegerLiteral(3),
                Token::RightBracket,
                Token::Arrow,
                Token::LeftParen,
                Token::Identifier("b".to_string()),
                Token::RightParen,
                Token::Eof
            ]
        );
    }

    // =====================================================
    // Error Tests
    // =====================================================

    #[test]
    fn test_unterminated_string() {
        let result = Lexer::tokenize("'unterminated");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unterminated string"));
    }

    #[test]
    fn test_unexpected_character() {
        let result = Lexer::tokenize("MATCH @invalid");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unexpected character"));
    }

    #[test]
    fn test_invalid_not_equal() {
        let result = Lexer::tokenize("!");
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_parameter() {
        let result = Lexer::tokenize("$");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("parameter name"));
    }

    // =====================================================
    // Token Display Tests
    // =====================================================

    #[test]
    fn test_token_display() {
        assert_eq!(format!("{}", Token::Match), "MATCH");
        assert_eq!(format!("{}", Token::Arrow), "->");
        assert_eq!(format!("{}", Token::Identifier("foo".to_string())), "foo");
        assert_eq!(
            format!("{}", Token::StringLiteral("bar".to_string())),
            "'bar'"
        );
        assert_eq!(format!("{}", Token::IntegerLiteral(42)), "42");
        assert_eq!(format!("{}", Token::FloatLiteral(2.71)), "2.71");
        assert_eq!(format!("{}", Token::Parameter("p".to_string())), "$p");
    }

    #[test]
    fn test_token_coverage() {
        let tokens = vec![
            // Keywords - Graph
            Token::Match,
            Token::Where,
            Token::Return,
            Token::Order,
            Token::By,
            Token::Limit,
            Token::Skip,
            Token::Distinct,
            Token::Count,
            Token::Asc,
            Token::Desc,
            // Keywords - Logical
            Token::And,
            Token::Or,
            Token::Not,
            Token::In,
            Token::Is,
            Token::Null,
            Token::True,
            Token::False,
            // Keywords - Vector
            Token::Similar,
            Token::To,
            Token::Using,
            Token::Find,
            Token::Rank,
            Token::Similarity,
            Token::Top,
            // Keywords - Temporal
            Token::As,
            Token::Of,
            Token::Between,
            // Keywords - String predicates
            Token::Exists,
            Token::Contains,
            Token::Starts,
            Token::Ends,
            Token::With,
            // Keywords - Distance metrics
            Token::Cosine,
            Token::Euclidean,
            Token::DotProduct,
            // Punctuation
            Token::LeftParen,
            Token::RightParen,
            Token::LeftBracket,
            Token::RightBracket,
            Token::LeftBrace,
            Token::RightBrace,
            Token::Colon,
            Token::Comma,
            Token::Dot,
            Token::Star,
            Token::Dash,
            // Arrow patterns
            Token::Arrow,
            Token::LeftArrow,
            // Comparison operators
            Token::Eq,
            Token::Ne,
            Token::Lt,
            Token::Le,
            Token::Gt,
            Token::Ge,
            // Literals
            Token::Identifier("id".to_string()),
            Token::StringLiteral("str".to_string()),
            Token::IntegerLiteral(42),
            Token::FloatLiteral(3.14),
            Token::Parameter("param".to_string()),
            // EOF
            Token::Eof,
        ];

        for token in tokens {
            // Test Clone
            let cloned = token.clone();
            // Test PartialEq
            assert_eq!(token, cloned);
            // Test Debug
            let _ = format!("{:?}", token);
            // Test Display
            let _ = format!("{}", token);
        }
    }
}
