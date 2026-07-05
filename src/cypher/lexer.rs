//! Cypher Query Language Lexer
//!
//! Hand-written tokenizer that converts a Cypher query string into a `Vec<Token>`.
//! This is the first stage of the Cypher pipeline, producing tokens that feed into
//! the recursive descent parser.
//!
//! # Design
//!
//! - **No external dependencies**: pure Rust, zero allocations beyond the output vec.
//! - **Case-insensitive keywords**: identifiers are upper-cased and compared to a keyword table.
//! - **Position tracking**: every token records its byte offset for error diagnostics.
//! - **Always ends with `Eof`**: the token stream is always terminated.

use super::CypherError;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// The kind of a Cypher token.
///
/// Covers keywords, operators, symbols, literals, parameters, identifiers,
/// and the end-of-input marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // -- Keywords: clauses --------------------------------------------------
    /// `MATCH`
    Match,
    /// `OPTIONAL` (always followed by `MATCH` in valid Cypher, but lexed separately
    /// so the parser can enforce the pairing)
    OptionalMatch,
    /// `WHERE`
    Where,
    /// `RETURN`
    Return,
    /// `WITH`
    With,
    /// `UNWIND`
    Unwind,

    // -- Keywords: ordering / projection ------------------------------------
    /// `ORDER`
    Order,
    /// `BY`
    By,
    /// `LIMIT`
    Limit,
    /// `SKIP`
    Skip,
    /// `AS`
    As,
    /// `DISTINCT`
    Distinct,

    // -- Keywords: logical --------------------------------------------------
    /// `AND`
    And,
    /// `OR`
    Or,
    /// `NOT`
    Not,
    /// `IN`
    In,
    /// `IS`
    Is,

    // -- Keywords: sort direction -------------------------------------------
    /// `ASC`
    Asc,
    /// `DESC`
    Desc,

    // -- Keywords: literals -------------------------------------------------
    /// `TRUE`
    True,
    /// `FALSE`
    False,
    /// `NULL`
    Null,

    // -- Keywords: string predicates ----------------------------------------
    /// `CONTAINS`
    Contains,
    /// `STARTS` (used in `STARTS WITH`)
    StartsWith,
    /// `ENDS` (used in `ENDS WITH`)
    EndsWith,

    // -- Keywords: aggregation ----------------------------------------------
    /// `COUNT`
    Count,
    /// `COLLECT`
    Collect,
    /// `AVG`
    Avg,
    /// `SUM`
    Sum,
    /// `MIN`
    Min,
    /// `MAX`
    Max,

    // -- Keywords: temporal -------------------------------------------------
    /// `OF`
    Of,
    /// `TIMESTAMP`
    Timestamp,
    /// `BETWEEN`
    Between,
    /// `FOR`
    For,
    /// `SYSTEM_TIME`
    SystemTime,
    /// `VALID_TIME`
    ValidTime,

    // -- Comparison operators -----------------------------------------------
    /// `=`
    Eq,
    /// `<>` or `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,

    // -- Symbols / punctuation ----------------------------------------------
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `,`
    Comma,
    /// `|`
    Pipe,
    /// `-`
    Dash,
    /// `->`
    Arrow,
    /// `<-`
    LeftArrow,
    /// `*`
    Star,
    /// `+`
    Plus,
    /// `/`
    Slash,
    /// `%`
    Percent,

    // -- Literals -----------------------------------------------------------
    /// An integer literal (e.g. `42`).
    IntegerLiteral,
    /// A floating-point literal (e.g. `3.14`).
    FloatLiteral,
    /// A string literal (`'hello'` or `"hello"`). The `text` field of the
    /// [`Token`] holds the *unescaped* content without surrounding quotes.
    StringLiteral,
    /// A parameter reference (`$name`). The `text` field holds the name
    /// *without* the leading `$`.
    Parameter,
    /// An identifier that did not match any keyword.
    Identifier,

    // -- End of input -------------------------------------------------------
    /// Marks the end of the token stream.
    Eof,
}

// ---------------------------------------------------------------------------
// Token
// ---------------------------------------------------------------------------

/// A single token produced by the Cypher lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// The source text of the token. For string literals this is the unescaped
    /// content (no quotes). For parameters this is the name without `$`.
    pub text: String,
    /// Byte offset in the original input where this token starts.
    pub position: usize,
}

impl Token {
    /// Create a new token.
    fn new(kind: TokenKind, text: impl Into<String>, position: usize) -> Self {
        Self {
            kind,
            text: text.into(),
            position,
        }
    }
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

/// Cypher query language lexer.
///
/// Converts a Cypher query string into a vector of [`Token`]s. This is the
/// entry point for the lexer; use [`CypherLexer::tokenize`] to lex a query.
///
/// # Examples
///
/// ```rust,ignore
/// use aletheiadb::cypher::lexer::{CypherLexer, TokenKind};
///
/// let tokens = CypherLexer::tokenize("MATCH (n) RETURN n").unwrap();
/// assert_eq!(tokens[0].kind, TokenKind::Match);
/// ```
pub struct CypherLexer<'a> {
    input: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    position: usize,
}

impl<'a> CypherLexer<'a> {
    /// Tokenize the complete input, returning a vector that always ends with
    /// [`TokenKind::Eof`].
    pub fn tokenize(input: &str) -> Result<Vec<Token>, CypherError> {
        let mut lexer = CypherLexer {
            input,
            chars: input.char_indices().peekable(),
            position: 0,
        };

        let mut tokens = Vec::with_capacity(input.len() / 4 + 1);
        loop {
            let tok = lexer.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    // -----------------------------------------------------------------------
    // Main dispatch
    // -----------------------------------------------------------------------

    fn next_token(&mut self) -> Result<Token, CypherError> {
        self.skip_whitespace_and_comments();

        let Some(&(pos, ch)) = self.chars.peek() else {
            return Ok(Token::new(TokenKind::Eof, "", self.input.len()));
        };
        self.position = pos;

        match ch {
            // -- Single-char symbols ----------------------------------------
            '(' => {
                self.advance();
                Ok(Token::new(TokenKind::LParen, "(", pos))
            }
            ')' => {
                self.advance();
                Ok(Token::new(TokenKind::RParen, ")", pos))
            }
            '[' => {
                self.advance();
                Ok(Token::new(TokenKind::LBracket, "[", pos))
            }
            ']' => {
                self.advance();
                Ok(Token::new(TokenKind::RBracket, "]", pos))
            }
            '{' => {
                self.advance();
                Ok(Token::new(TokenKind::LBrace, "{", pos))
            }
            '}' => {
                self.advance();
                Ok(Token::new(TokenKind::RBrace, "}", pos))
            }
            ':' => {
                self.advance();
                Ok(Token::new(TokenKind::Colon, ":", pos))
            }
            ',' => {
                self.advance();
                Ok(Token::new(TokenKind::Comma, ",", pos))
            }
            '*' => {
                self.advance();
                Ok(Token::new(TokenKind::Star, "*", pos))
            }
            '+' => {
                self.advance();
                Ok(Token::new(TokenKind::Plus, "+", pos))
            }
            '%' => {
                self.advance();
                Ok(Token::new(TokenKind::Percent, "%", pos))
            }
            '|' => {
                self.advance();
                Ok(Token::new(TokenKind::Pipe, "|", pos))
            }

            // -- Dot / DotDot ----------------------------------------------
            '.' => self.read_dot(pos),

            // -- Equals ----------------------------------------------------
            '=' => {
                self.advance();
                Ok(Token::new(TokenKind::Eq, "=", pos))
            }

            // -- Multi-char operators starting with specific chars ----------
            '-' => self.read_dash(pos),
            '<' => self.read_less_than(pos),
            '>' => self.read_greater_than(pos),
            '!' => self.read_bang(pos),
            '/' => self.read_slash(pos),

            // -- String literals -------------------------------------------
            '\'' | '"' => self.read_string(pos),

            // -- Parameter -------------------------------------------------
            '$' => self.read_parameter(pos),

            // -- Numbers ---------------------------------------------------
            '0'..='9' => self.read_number(pos),

            // -- Identifiers / keywords ------------------------------------
            'a'..='z' | 'A'..='Z' | '_' => self.read_identifier_or_keyword(pos),

            _ => Err(self.lex_error(pos, format!("Unexpected character: '{ch}'"))),
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn advance(&mut self) -> Option<(usize, char)> {
        self.chars.next()
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().map(|&(_, c)| c)
    }

    fn lex_error(&self, position: usize, message: String) -> CypherError {
        CypherError::LexError { position, message }
    }

    // -----------------------------------------------------------------------
    // Whitespace & comments
    // -----------------------------------------------------------------------

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            // Skip whitespace
            while let Some(&(_, ch)) = self.chars.peek() {
                if ch.is_whitespace() {
                    self.advance();
                } else {
                    break;
                }
            }

            // Check for // line comment
            if let Some(&(_, '/')) = self.chars.peek() {
                let mut lookahead = self.chars.clone();
                lookahead.next();
                if let Some(&(_, '/')) = lookahead.peek() {
                    // Consume the two slashes
                    self.advance();
                    self.advance();
                    // Skip to end of line
                    while let Some(&(_, ch)) = self.chars.peek() {
                        if ch == '\n' {
                            self.advance();
                            break;
                        }
                        self.advance();
                    }
                    continue;
                }
            }

            break;
        }
    }

    // -----------------------------------------------------------------------
    // Dot / DotDot
    // -----------------------------------------------------------------------

    fn read_dot(&mut self, start: usize) -> Result<Token, CypherError> {
        self.advance(); // consume first '.'
        if let Some(&(_, '.')) = self.chars.peek() {
            self.advance(); // consume second '.'
            Ok(Token::new(TokenKind::DotDot, "..", start))
        } else {
            Ok(Token::new(TokenKind::Dot, ".", start))
        }
    }

    // -----------------------------------------------------------------------
    // Dash / Arrow
    // -----------------------------------------------------------------------

    fn read_dash(&mut self, start: usize) -> Result<Token, CypherError> {
        self.advance(); // consume '-'
        if let Some(&(_, '>')) = self.chars.peek() {
            self.advance();
            Ok(Token::new(TokenKind::Arrow, "->", start))
        } else {
            Ok(Token::new(TokenKind::Dash, "-", start))
        }
    }

    // -----------------------------------------------------------------------
    // Less-than family:  <  <=  <>  <-
    // -----------------------------------------------------------------------

    fn read_less_than(&mut self, start: usize) -> Result<Token, CypherError> {
        self.advance(); // consume '<'
        match self.peek_char() {
            Some('=') => {
                self.advance();
                Ok(Token::new(TokenKind::Le, "<=", start))
            }
            Some('>') => {
                self.advance();
                Ok(Token::new(TokenKind::Ne, "<>", start))
            }
            Some('-') => {
                self.advance();
                Ok(Token::new(TokenKind::LeftArrow, "<-", start))
            }
            _ => Ok(Token::new(TokenKind::Lt, "<", start)),
        }
    }

    // -----------------------------------------------------------------------
    // Greater-than family:  >  >=
    // -----------------------------------------------------------------------

    fn read_greater_than(&mut self, start: usize) -> Result<Token, CypherError> {
        self.advance(); // consume '>'
        if let Some('=') = self.peek_char() {
            self.advance();
            Ok(Token::new(TokenKind::Ge, ">=", start))
        } else {
            Ok(Token::new(TokenKind::Gt, ">", start))
        }
    }

    // -----------------------------------------------------------------------
    // Bang:  !=
    // -----------------------------------------------------------------------

    fn read_bang(&mut self, start: usize) -> Result<Token, CypherError> {
        self.advance(); // consume '!'
        if let Some('=') = self.peek_char() {
            self.advance();
            Ok(Token::new(TokenKind::Ne, "!=", start))
        } else {
            Err(self.lex_error(start, "Expected '=' after '!'".to_string()))
        }
    }

    // -----------------------------------------------------------------------
    // Slash (might be division, comments were already consumed)
    // -----------------------------------------------------------------------

    fn read_slash(&mut self, start: usize) -> Result<Token, CypherError> {
        self.advance(); // consume '/'
        Ok(Token::new(TokenKind::Slash, "/", start))
    }

    // -----------------------------------------------------------------------
    // String literals
    // -----------------------------------------------------------------------

    fn read_string(&mut self, start: usize) -> Result<Token, CypherError> {
        let (_, quote) = self.advance().ok_or_else(|| {
            self.lex_error(start, "Unexpected EOF while reading string".to_string())
        })?; // consume opening quote
        let mut value = String::new();

        loop {
            match self.advance() {
                Some((_, ch)) if ch == quote => {
                    // End of string
                    return Ok(Token::new(TokenKind::StringLiteral, value, start));
                }
                Some((_, '\\')) => {
                    // Escape sequence
                    match self.advance() {
                        Some((_, c)) if c == quote => value.push(c),
                        Some((_, '\\')) => value.push('\\'),
                        Some((_, 'n')) => value.push('\n'),
                        Some((_, 't')) => value.push('\t'),
                        Some((_, 'r')) => value.push('\r'),
                        Some((_, other)) => {
                            value.push('\\');
                            value.push(other);
                        }
                        None => {
                            return Err(
                                self.lex_error(start, "Unterminated string literal".to_string())
                            );
                        }
                    }
                }
                Some((_, ch)) => value.push(ch),
                None => {
                    return Err(self.lex_error(start, "Unterminated string literal".to_string()));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Parameters
    // -----------------------------------------------------------------------

    fn read_parameter(&mut self, start: usize) -> Result<Token, CypherError> {
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
            return Err(self.lex_error(start, "Expected parameter name after '$'".to_string()));
        }
        Ok(Token::new(TokenKind::Parameter, name, start))
    }

    // -----------------------------------------------------------------------
    // Numbers
    // -----------------------------------------------------------------------

    fn read_number(&mut self, start: usize) -> Result<Token, CypherError> {
        let mut is_float = false;

        // Consume leading digits
        while let Some(&(_, ch)) = self.chars.peek() {
            if ch.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }

        // Check for decimal point followed by digit (not `..` which is DotDot)
        if let Some(&(_, '.')) = self.chars.peek() {
            let mut lookahead = self.chars.clone();
            lookahead.next(); // skip the '.'
            match lookahead.peek() {
                Some(&(_, ch)) if ch.is_ascii_digit() => {
                    is_float = true;
                    self.advance(); // consume '.'
                    while let Some(&(_, ch)) = self.chars.peek() {
                        if ch.is_ascii_digit() {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                // '.' followed by another '.' => DotDot, don't consume
                _ => {}
            }
        }

        let end = self
            .chars
            .peek()
            .map(|&(pos, _)| pos)
            .unwrap_or(self.input.len());
        let text = &self.input[start..end];

        let kind = if is_float {
            TokenKind::FloatLiteral
        } else {
            TokenKind::IntegerLiteral
        };
        Ok(Token::new(kind, text, start))
    }

    // -----------------------------------------------------------------------
    // Identifiers / Keywords
    // -----------------------------------------------------------------------

    fn read_identifier_or_keyword(&mut self, start: usize) -> Result<Token, CypherError> {
        while let Some(&(_, ch)) = self.chars.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.advance();
            } else {
                break;
            }
        }

        let end = self
            .chars
            .peek()
            .map(|&(pos, _)| pos)
            .unwrap_or(self.input.len());
        let text = &self.input[start..end];

        let kind = match text.to_uppercase().as_str() {
            // Clauses
            "MATCH" => TokenKind::Match,
            "OPTIONAL" => TokenKind::OptionalMatch,
            "WHERE" => TokenKind::Where,
            "RETURN" => TokenKind::Return,
            "WITH" => TokenKind::With,
            "UNWIND" => TokenKind::Unwind,

            // Ordering / projection
            "ORDER" => TokenKind::Order,
            "BY" => TokenKind::By,
            "LIMIT" => TokenKind::Limit,
            "SKIP" => TokenKind::Skip,
            "AS" => TokenKind::As,
            "DISTINCT" => TokenKind::Distinct,

            // Logical
            "AND" => TokenKind::And,
            "OR" => TokenKind::Or,
            "NOT" => TokenKind::Not,
            "IN" => TokenKind::In,
            "IS" => TokenKind::Is,

            // Sort direction
            "ASC" => TokenKind::Asc,
            "DESC" => TokenKind::Desc,

            // Literals
            "TRUE" => TokenKind::True,
            "FALSE" => TokenKind::False,
            "NULL" => TokenKind::Null,

            // String predicates
            "CONTAINS" => TokenKind::Contains,
            "STARTS" => TokenKind::StartsWith,
            "ENDS" => TokenKind::EndsWith,

            // Aggregation
            "COUNT" => TokenKind::Count,
            "COLLECT" => TokenKind::Collect,
            "AVG" => TokenKind::Avg,
            "SUM" => TokenKind::Sum,
            "MIN" => TokenKind::Min,
            "MAX" => TokenKind::Max,

            // Temporal
            "OF" => TokenKind::Of,
            "TIMESTAMP" => TokenKind::Timestamp,
            "BETWEEN" => TokenKind::Between,
            "FOR" => TokenKind::For,
            "SYSTEM_TIME" => TokenKind::SystemTime,
            "VALID_TIME" => TokenKind::ValidTime,

            // Not a keyword
            _ => TokenKind::Identifier,
        };

        Ok(Token::new(kind, text, start))
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn keyword_lookup_exhaustive() {
        // Ensure every keyword string maps to a non-Identifier kind.
        let keywords = [
            "MATCH",
            "OPTIONAL",
            "WHERE",
            "RETURN",
            "WITH",
            "UNWIND",
            "ORDER",
            "BY",
            "LIMIT",
            "SKIP",
            "AS",
            "DISTINCT",
            "AND",
            "OR",
            "NOT",
            "IN",
            "IS",
            "ASC",
            "DESC",
            "TRUE",
            "FALSE",
            "NULL",
            "CONTAINS",
            "STARTS",
            "ENDS",
            "COUNT",
            "COLLECT",
            "AVG",
            "SUM",
            "MIN",
            "MAX",
            "OF",
            "TIMESTAMP",
            "BETWEEN",
            "FOR",
            "SYSTEM_TIME",
            "VALID_TIME",
        ];
        for kw in keywords {
            let tokens = CypherLexer::tokenize(kw).unwrap();
            assert_ne!(
                tokens[0].kind,
                TokenKind::Identifier,
                "{kw} should be a keyword, not an identifier"
            );
        }
    }

    #[test]
    fn test_sentry_lexer_read_string_eof() {
        let lexer = CypherLexer::tokenize("'");
        assert!(lexer.is_err());
    }
}
