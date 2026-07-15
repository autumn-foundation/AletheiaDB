//! Minimal, panic-free EDN reader for the XTDB / Datomic importers (Issue #3384).
//!
//! EDN (Extensible Data Notation) is the native serialization of both XTDB and
//! Datomic: a Clojure export script emits it directly and it preserves
//! keywords, `#inst`, and `#uuid` losslessly (unlike a JSON round-trip). This
//! module ships **only** the subset the two migration artifacts need — it is
//! deliberately *not* a full Clojure reader.
//!
//! # Supported
//!
//! - `nil`, `true`, `false`
//! - integers (optional sign, decimal digits) and floats (sign / digits / `.` /
//!   `e|E` exponent / trailing `M` decimal marker, stripped)
//! - strings `"..."` with escapes `\" \\ \n \t \r \/ \uXXXX`
//! - keywords `:kw`, `:ns/kw`, `::alias/kw` (the leading colon(s) are stripped;
//!   a namespaced keyword keeps its `ns/name` text so the caller can normalize)
//! - symbols (bare tokens, retained verbatim)
//! - collections: vector `[]`, list `()`, map `{}`, set `#{}`
//! - `;` line comments and `#_` datum-discard
//! - tagged literals `#inst "..."` (→ epoch micros) and `#uuid "..."` (→ the
//!   uuid string); **any other** `#tag <form>` is tolerated — the tag is
//!   discarded and the inner form retained, never fatal
//!
//! # Out of scope (typed [`EdnError::Unsupported`], never a panic)
//!
//! `#=` eval, ratios (`1/2`), radix / bigint `N` suffixes, char literals
//! `\c`, regex `#"..."`, metadata `^{}`, namespaced-map `#:ns{...}`, and reader
//! conditionals `#?()`. Trailing `M` on a float is accepted (precision loss is
//! documented); a bigint `N` suffix is rejected.
//!
//! # Robustness
//!
//! The reader tracks a 1-based line/column and attaches it to every error. It
//! **never panics** on malformed input: recursion depth is bounded
//! ([`MAX_DEPTH`]) so a pathological deeply-nested collection returns
//! [`EdnError::TooDeep`] instead of overflowing the stack, and no indexing can
//! run off the end of the buffer.

use std::fmt;

/// Maximum nesting depth for collections before the reader bails with
/// [`EdnError::TooDeep`]. Mirrors the Cypher importer's `MAX_VALUE_DEPTH` cap:
/// adversarial input must fail loudly, not overflow the stack.
pub(crate) const MAX_DEPTH: usize = 256;

/// A parsed EDN value (the subset the importers need).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Edn {
    /// `nil`.
    Nil,
    /// `true` / `false`.
    Bool(bool),
    /// A 64-bit integer.
    Int(i64),
    /// A double-precision float.
    Float(f64),
    /// A string literal (escapes already decoded).
    Str(String),
    /// A keyword, stored **without** the leading `:`; a namespaced keyword keeps
    /// its `ns/name` text (`:xtdb.api/valid-time` → `"xtdb.api/valid-time"`).
    Keyword(String),
    /// A bare symbol, retained verbatim.
    Symbol(String),
    /// A vector `[...]`.
    Vector(Vec<Edn>),
    /// A list `(...)`.
    List(Vec<Edn>),
    /// A map `{...}`, insertion-ordered; keys are compared structurally.
    Map(Vec<(Edn, Edn)>),
    /// A set `#{...}`.
    Set(Vec<Edn>),
    /// `#inst "..."` resolved to epoch **microseconds** (UTC).
    Inst(i64),
    /// `#uuid "..."` retained as its string form.
    Uuid(String),
}

impl Edn {
    /// Borrow the value as a keyword's local text, if it is a keyword.
    pub(crate) fn as_keyword(&self) -> Option<&str> {
        match self {
            Edn::Keyword(k) => Some(k.as_str()),
            _ => None,
        }
    }

    /// Borrow the value as a string, if it is one.
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Edn::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Borrow the value as a map's entries, if it is a map.
    pub(crate) fn as_map(&self) -> Option<&[(Edn, Edn)]> {
        match self {
            Edn::Map(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }

    /// Borrow the value as a vector's / list's elements, if it is one of those.
    pub(crate) fn as_seq(&self) -> Option<&[Edn]> {
        match self {
            Edn::Vector(v) | Edn::List(v) | Edn::Set(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Look up a map entry by the local name of a keyword key. The lookup
    /// **ignores namespaces**: a `namespace/name` key matches on `name`, so
    /// `:xtdb.api/valid-time` and `:xt/valid-time` both resolve for `"valid-time"`.
    pub(crate) fn map_get_local(&self, local: &str) -> Option<&Edn> {
        let entries = self.as_map()?;
        entries.iter().find_map(|(k, v)| match k {
            Edn::Keyword(name) => (keyword_local(name) == local).then_some(v),
            _ => None,
        })
    }

    /// A short human token naming the value's kind, for error messages.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Edn::Nil => "nil",
            Edn::Bool(_) => "bool",
            Edn::Int(_) => "int",
            Edn::Float(_) => "float",
            Edn::Str(_) => "string",
            Edn::Keyword(_) => "keyword",
            Edn::Symbol(_) => "symbol",
            Edn::Vector(_) => "vector",
            Edn::List(_) => "list",
            Edn::Map(_) => "map",
            Edn::Set(_) => "set",
            Edn::Inst(_) => "inst",
            Edn::Uuid(_) => "uuid",
        }
    }
}

/// Return the local (namespace-stripped) portion of a keyword/symbol text.
/// `"xtdb.api/valid-time"` → `"valid-time"`; `"type"` → `"type"`.
pub(crate) fn keyword_local(name: &str) -> &str {
    match name.rsplit_once('/') {
        Some((_, local)) => local,
        None => name,
    }
}

/// A typed EDN parse error, always carrying a 1-based source position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EdnError {
    /// The input ended while a value/collection/string was still open.
    UnexpectedEof,
    /// An unexpected character was encountered mid-parse.
    UnexpectedChar {
        found: char,
        line: usize,
        col: usize,
    },
    /// A string literal was not closed before EOF or newline handling.
    UnterminatedString { line: usize, col: usize },
    /// A closing delimiter did not match the open one (`[` closed by `}`), or a
    /// stray closer with no opener.
    UnbalancedDelimiter {
        expected: char,
        found: char,
        line: usize,
        col: usize,
    },
    /// An unknown / malformed string escape (`\q`, truncated `\u12`).
    BadEscape { line: usize, col: usize },
    /// A `#inst` / `#uuid` (or other) tagged literal whose payload was invalid.
    BadTaggedLiteral {
        tag: String,
        reason: String,
        line: usize,
        col: usize,
    },
    /// A numeric literal that does not fit / is malformed (e.g. a 200-digit int
    /// beyond `i64`, or a bigint `N` suffix).
    BadNumber {
        reason: String,
        line: usize,
        col: usize,
    },
    /// A construct the reader deliberately does not support (ratio, regex, `#=`,
    /// metadata, namespaced-map, reader conditional).
    Unsupported {
        construct: String,
        line: usize,
        col: usize,
    },
    /// Collections nested deeper than [`MAX_DEPTH`].
    TooDeep { line: usize, col: usize },
    /// Extra non-whitespace data after a single top-level value (`parse_one`).
    TrailingData { line: usize, col: usize },
}

impl fmt::Display for EdnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdnError::UnexpectedEof => write!(f, "unexpected end of EDN input"),
            EdnError::UnexpectedChar { found, line, col } => {
                write!(f, "unexpected character '{found}' at {line}:{col}")
            }
            EdnError::UnterminatedString { line, col } => {
                write!(f, "unterminated string at {line}:{col}")
            }
            EdnError::UnbalancedDelimiter {
                expected,
                found,
                line,
                col,
            } => write!(
                f,
                "unbalanced delimiter at {line}:{col}: expected '{expected}', found '{found}'"
            ),
            EdnError::BadEscape { line, col } => write!(f, "bad string escape at {line}:{col}"),
            EdnError::BadTaggedLiteral {
                tag,
                reason,
                line,
                col,
            } => write!(f, "bad #{tag} literal at {line}:{col}: {reason}"),
            EdnError::BadNumber { reason, line, col } => {
                write!(f, "bad number at {line}:{col}: {reason}")
            }
            EdnError::Unsupported {
                construct,
                line,
                col,
            } => write!(f, "unsupported EDN construct '{construct}' at {line}:{col}"),
            EdnError::TooDeep { line, col } => {
                write!(f, "EDN nesting too deep (>{MAX_DEPTH}) at {line}:{col}")
            }
            EdnError::TrailingData { line, col } => {
                write!(f, "trailing data after top-level value at {line}:{col}")
            }
        }
    }
}

impl std::error::Error for EdnError {}

/// The line/column reported for a given error, when it carries one. Used by
/// tests to assert precise positions.
impl EdnError {
    #[cfg(test)]
    pub(crate) fn position(&self) -> Option<(usize, usize)> {
        match self {
            EdnError::UnexpectedEof => None,
            EdnError::UnexpectedChar { line, col, .. }
            | EdnError::UnterminatedString { line, col }
            | EdnError::UnbalancedDelimiter { line, col, .. }
            | EdnError::BadEscape { line, col }
            | EdnError::BadTaggedLiteral { line, col, .. }
            | EdnError::BadNumber { line, col, .. }
            | EdnError::Unsupported { line, col, .. }
            | EdnError::TooDeep { line, col }
            | EdnError::TrailingData { line, col } => Some((*line, *col)),
        }
    }
}

/// A position-tracking EDN reader over an owned char buffer.
struct Parser {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        Parser {
            chars: input.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    /// Consume and return the next char, advancing the 1-based line/col.
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn pos_tuple(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    /// Skip whitespace (including `,`) and `;` line comments.
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == ',' {
                self.bump();
            } else if c == ';' {
                // Line comment: consume to end of line.
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    self.bump();
                }
            } else {
                break;
            }
        }
    }

    /// Parse a single value at the current position, honoring `#_` datum
    /// discards that precede it.
    fn parse_value(&mut self, depth: usize) -> Result<Edn, EdnError> {
        loop {
            self.skip_ws();
            // Handle `#_` datum discard: skip the next form and continue.
            if self.peek() == Some('#') && self.peek_at(1) == Some('_') {
                self.bump();
                self.bump();
                // Discard exactly one following form.
                self.parse_value(depth)?;
                continue;
            }
            break;
        }

        let (line, col) = self.pos_tuple();
        let c = self.peek().ok_or(EdnError::UnexpectedEof)?;
        match c {
            '[' => self.parse_seq('[', ']', depth),
            '(' => self.parse_seq('(', ')', depth),
            '{' => self.parse_map(depth),
            '"' => self.parse_string(),
            ':' => self.parse_keyword(),
            '#' => self.parse_dispatch(depth),
            ']' | '}' | ')' => Err(EdnError::UnbalancedDelimiter {
                expected: 'x',
                found: c,
                line,
                col,
            }),
            '\\' => Err(EdnError::Unsupported {
                construct: "char literal".to_string(),
                line,
                col,
            }),
            '^' => Err(EdnError::Unsupported {
                construct: "metadata".to_string(),
                line,
                col,
            }),
            c if c == '-' || c == '+' || c.is_ascii_digit() => self.parse_number(),
            c if is_symbol_start(c) => self.parse_symbol_or_atom(),
            _ => Err(EdnError::UnexpectedChar {
                found: c,
                line,
                col,
            }),
        }
    }

    /// Parse a `[...]` vector or `(...)` list.
    fn parse_seq(&mut self, open: char, close: char, depth: usize) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        if depth >= MAX_DEPTH {
            return Err(EdnError::TooDeep { line, col });
        }
        self.bump(); // consume opener
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(EdnError::UnexpectedEof),
                Some(c) if c == close => {
                    self.bump();
                    break;
                }
                Some(c) if c == ']' || c == '}' || c == ')' => {
                    let (l, cc) = self.pos_tuple();
                    return Err(EdnError::UnbalancedDelimiter {
                        expected: close,
                        found: c,
                        line: l,
                        col: cc,
                    });
                }
                Some(_) => items.push(self.parse_value(depth + 1)?),
            }
        }
        Ok(if open == '[' {
            Edn::Vector(items)
        } else {
            Edn::List(items)
        })
    }

    /// Parse a `{...}` map.
    fn parse_map(&mut self, depth: usize) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        if depth >= MAX_DEPTH {
            return Err(EdnError::TooDeep { line, col });
        }
        self.bump(); // consume '{'
        let mut entries = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(EdnError::UnexpectedEof),
                Some('}') => {
                    self.bump();
                    break;
                }
                Some(c) if c == ']' || c == ')' => {
                    let (l, cc) = self.pos_tuple();
                    return Err(EdnError::UnbalancedDelimiter {
                        expected: '}',
                        found: c,
                        line: l,
                        col: cc,
                    });
                }
                Some(_) => {
                    let key = self.parse_value(depth + 1)?;
                    self.skip_ws();
                    if self.peek().is_none() {
                        return Err(EdnError::UnexpectedEof);
                    }
                    if self.peek() == Some('}') {
                        // Odd number of forms in a map.
                        let (l, cc) = self.pos_tuple();
                        return Err(EdnError::UnexpectedChar {
                            found: '}',
                            line: l,
                            col: cc,
                        });
                    }
                    let value = self.parse_value(depth + 1)?;
                    entries.push((key, value));
                }
            }
        }
        Ok(Edn::Map(entries))
    }

    /// Parse a `#`-dispatch form: `#{` set, `#inst`, `#uuid`, `#_` (handled
    /// earlier), or an unknown `#tag <form>` (tolerated), or an unsupported
    /// `#"..."` / `#?(...)` / `#:ns{...}` / `#=` construct.
    fn parse_dispatch(&mut self, depth: usize) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        self.bump(); // consume '#'
        match self.peek() {
            None => Err(EdnError::UnexpectedEof),
            Some('{') => self.parse_set(depth),
            Some('"') => Err(EdnError::Unsupported {
                construct: "regex literal".to_string(),
                line,
                col,
            }),
            Some('?') => Err(EdnError::Unsupported {
                construct: "reader conditional".to_string(),
                line,
                col,
            }),
            Some('=') => Err(EdnError::Unsupported {
                construct: "eval literal".to_string(),
                line,
                col,
            }),
            Some(':') => Err(EdnError::Unsupported {
                construct: "namespaced map".to_string(),
                line,
                col,
            }),
            Some('_') => {
                // Should have been handled in parse_value, but be defensive.
                self.bump();
                self.parse_value(depth)?;
                self.parse_value(depth)
            }
            Some(c) if is_symbol_start(c) => {
                let tag = self.read_token();
                self.skip_ws();
                let (tline, tcol) = (line, col);
                let inner = self.parse_value(depth + 1)?;
                self.apply_tag(&tag, inner, tline, tcol)
            }
            Some(c) => Err(EdnError::UnexpectedChar {
                found: c,
                line,
                col,
            }),
        }
    }

    /// Parse a `#{...}` set (the `#` has been consumed; we are at `{`).
    fn parse_set(&mut self, depth: usize) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        if depth >= MAX_DEPTH {
            return Err(EdnError::TooDeep { line, col });
        }
        self.bump(); // consume '{'
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(EdnError::UnexpectedEof),
                Some('}') => {
                    self.bump();
                    break;
                }
                Some(c) if c == ']' || c == ')' => {
                    let (l, cc) = self.pos_tuple();
                    return Err(EdnError::UnbalancedDelimiter {
                        expected: '}',
                        found: c,
                        line: l,
                        col: cc,
                    });
                }
                Some(_) => items.push(self.parse_value(depth + 1)?),
            }
        }
        Ok(Edn::Set(items))
    }

    /// Apply a tag to its already-parsed inner form.
    fn apply_tag(&self, tag: &str, inner: Edn, line: usize, col: usize) -> Result<Edn, EdnError> {
        match tag {
            "inst" => {
                let s = inner.as_str().ok_or_else(|| EdnError::BadTaggedLiteral {
                    tag: "inst".to_string(),
                    reason: format!("expected a string, got {}", inner.kind()),
                    line,
                    col,
                })?;
                let ts = super::parse::parse_valid_time(s).map_err(|reason| {
                    EdnError::BadTaggedLiteral {
                        tag: "inst".to_string(),
                        reason,
                        line,
                        col,
                    }
                })?;
                Ok(Edn::Inst(ts.wallclock()))
            }
            "uuid" => {
                let s = inner.as_str().ok_or_else(|| EdnError::BadTaggedLiteral {
                    tag: "uuid".to_string(),
                    reason: format!("expected a string, got {}", inner.kind()),
                    line,
                    col,
                })?;
                Ok(Edn::Uuid(s.to_string()))
            }
            // Any other tag (e.g. `#crux/id`, `#audit/id`) is tolerated: the tag
            // is discarded and the inner form retained (never fatal).
            _ => Ok(inner),
        }
    }

    /// Parse a `"..."` string, decoding escapes.
    fn parse_string(&mut self) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        self.bump(); // consume opening quote
        let mut out = String::new();
        loop {
            let c = match self.bump() {
                Some(c) => c,
                None => return Err(EdnError::UnterminatedString { line, col }),
            };
            match c {
                '"' => return Ok(Edn::Str(out)),
                '\\' => {
                    let (eline, ecol) = self.pos_tuple();
                    let esc = self
                        .bump()
                        .ok_or(EdnError::UnterminatedString { line, col })?;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000C}'),
                        'u' => {
                            let mut code: u32 = 0;
                            for _ in 0..4 {
                                let h = self.bump().ok_or(EdnError::BadEscape {
                                    line: eline,
                                    col: ecol,
                                })?;
                                let digit = h.to_digit(16).ok_or(EdnError::BadEscape {
                                    line: eline,
                                    col: ecol,
                                })?;
                                code = code * 16 + digit;
                            }
                            let ch = char::from_u32(code).ok_or(EdnError::BadEscape {
                                line: eline,
                                col: ecol,
                            })?;
                            out.push(ch);
                        }
                        _ => {
                            return Err(EdnError::BadEscape {
                                line: eline,
                                col: ecol,
                            });
                        }
                    }
                }
                _ => out.push(c),
            }
        }
    }

    /// Parse a keyword: one or two leading colons, then the token.
    fn parse_keyword(&mut self) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        self.bump(); // consume first ':'
        // `::alias/kw` auto-resolved keyword — consume the second colon; we keep
        // the local+ns text and let the caller normalize by local name.
        if self.peek() == Some(':') {
            self.bump();
        }
        let token = self.read_token();
        if token.is_empty() {
            return Err(EdnError::UnexpectedChar {
                found: self.peek().unwrap_or(':'),
                line,
                col,
            });
        }
        Ok(Edn::Keyword(token))
    }

    /// Parse a bare symbol, or the atoms `nil` / `true` / `false`.
    fn parse_symbol_or_atom(&mut self) -> Result<Edn, EdnError> {
        let token = self.read_token();
        match token.as_str() {
            "nil" => Ok(Edn::Nil),
            "true" => Ok(Edn::Bool(true)),
            "false" => Ok(Edn::Bool(false)),
            _ => Ok(Edn::Symbol(token)),
        }
    }

    /// Parse an integer or float literal.
    fn parse_number(&mut self) -> Result<Edn, EdnError> {
        let (line, col) = self.pos_tuple();
        let token = self.read_token();
        // A ratio like `1/2` is unsupported.
        if token.contains('/') {
            return Err(EdnError::Unsupported {
                construct: "ratio".to_string(),
                line,
                col,
            });
        }
        let is_float = token.contains('.')
            || token.contains('e')
            || token.contains('E')
            || token.ends_with('M');
        if is_float {
            // Strip a trailing `M` (BigDecimal marker); precision loss is
            // documented in the module header.
            let cleaned = token.strip_suffix('M').unwrap_or(&token);
            cleaned
                .parse::<f64>()
                .map(Edn::Float)
                .map_err(|_| EdnError::BadNumber {
                    reason: format!("invalid float '{token}'"),
                    line,
                    col,
                })
        } else if let Some(stripped) = token.strip_suffix('N') {
            // Bigint literal — out of scope; report rather than silently
            // truncating (a value that fits i64 without the marker still errors,
            // making the loss explicit).
            let _ = stripped;
            Err(EdnError::BadNumber {
                reason: "arbitrary-precision integer 'N' suffix is unsupported".to_string(),
                line,
                col,
            })
        } else {
            token
                .parse::<i64>()
                .map(Edn::Int)
                .map_err(|_| EdnError::BadNumber {
                    reason: format!("integer '{token}' does not fit in i64"),
                    line,
                    col,
                })
        }
    }

    /// Read a run of token characters (used for symbols, keywords, numbers,
    /// tags). Stops at whitespace, `,`, or a delimiter/dispatch character.
    fn read_token(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || is_token_terminator(c) {
                break;
            }
            out.push(c);
            self.bump();
        }
        out
    }
}

/// Characters that terminate a bare token.
fn is_token_terminator(c: char) -> bool {
    matches!(c, '[' | ']' | '(' | ')' | '{' | '}' | '"' | ';' | ',')
}

/// Whether `c` can start a symbol/atom (letters, and the punctuation Clojure
/// allows in symbols). Excludes digits/sign (numbers), `:` (keywords), and the
/// structural characters.
fn is_symbol_start(c: char) -> bool {
    c.is_alphabetic()
        || matches!(
            c,
            '.' | '*' | '+' | '!' | '-' | '_' | '?' | '$' | '%' | '&' | '=' | '<' | '>'
        )
}

/// Parse **exactly one** top-level EDN value, rejecting trailing data.
pub(crate) fn parse_one(input: &str) -> Result<Edn, EdnError> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value(0)?;
    parser.skip_ws();
    if parser.peek().is_some() {
        let (line, col) = parser.pos_tuple();
        return Err(EdnError::TrailingData { line, col });
    }
    Ok(value)
}

/// Parse a top-level EDN **vector** `[...]` and return its elements. The XTDB
/// entity-history export and the Datomic datom stream are both a single
/// top-level vector; this is the artifact entry point.
pub(crate) fn parse_top_vector(input: &str) -> Result<Vec<Edn>, EdnError> {
    match parse_one(input)? {
        Edn::Vector(items) | Edn::List(items) => Ok(items),
        other => Err(EdnError::Unsupported {
            construct: format!("top-level {} (expected a vector)", other.kind()),
            line: 1,
            col: 1,
        }),
    }
}
