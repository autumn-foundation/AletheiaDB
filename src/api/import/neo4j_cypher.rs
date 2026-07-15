//! Neo4j **APOC Cypher-script dump** importer (Issue #3356).
//!
//! Loads the `create`-format output of `apoc.export.cypher.all` — a stream of
//! `CREATE`/`MATCH ... CREATE` Cypher statements framed by `:begin`/`:commit`
//! transaction markers — into AletheiaDB, reusing the #3211 bulk-import spine
//! (`import_nodes_with` / `import_edges_with`) and the Neo4j fidelity report,
//! provenance, and label-strategy machinery from the sibling CSV importer
//! ([`super::neo4j`]).
//!
//! It is a **self-contained** hand-written parser: a statement splitter, a
//! Cypher-literal value parser, and node/relationship pattern parsers, all in
//! this module. It does **not** depend on the openCypher query engine (the
//! `cypher` feature); the apoc `create` grammar is small and fixed, so parsing
//! it directly keeps the importer available in every build.
//!
//! # Supported constructs
//!
//! - `:begin` / `:commit` / `:rollback` transaction markers (no-op delimiters).
//! - Node `CREATE (:Label {..})`, including multi-label `(:A:B:C {..})`, the
//!   backtick-quoted `` (:`Label` {`prop`: ..}) `` form, and apoc's synthetic
//!   `` :`UNIQUE IMPORT LABEL` `` + `` `UNIQUE IMPORT ID` `` key scheme.
//! - Relationship `MATCH (a {key}), (b {key}) CREATE (a)-[:TYPE {..}]->(b)`,
//!   resolving endpoints by the matched key property.
//! - The apoc-optimized `UNWIND [{..},..] AS row CREATE (n:Label) SET n += row.properties`
//!   node batch form and its relationship `MATCH ... CREATE ... SET r += row.properties`
//!   variant.
//! - Cypher literal values: single/double-quoted strings (with `\n \t \r \b \f
//!   \\ \' \" \/ \uXXXX` escapes), integers, floats (incl. exponent), booleans,
//!   `null`, lists, nested maps (serialized to a JSON string), backtick
//!   identifiers, and the temporal constructors `datetime(..)`,
//!   `localdatetime(..)`, `date(..)` (→ epoch micros) and `time(..)` /
//!   `localtime(..)` (→ string). Numeric arrays become a dense
//!   [`PropertyValue::Vector`] **only** for a property named in
//!   [`Neo4jCypherOptions::vector_properties`], never silently.
//!
//! # Out of scope (reported, never silently dropped)
//!
//! User-defined `CREATE CONSTRAINT` / `CREATE INDEX`, `DROP`, `CALL`, `MERGE`,
//! spatial `point(..)`, `duration(..)`, and byte arrays are enumerated in the
//! fidelity report's `unsupported` list with counts (and set `zero_loss =
//! false`). `--strict-types` promotes an unsupported *value* to a hard error.
//! apoc's own `UNIQUE IMPORT` scaffolding (the constraint it creates/drops and
//! the `REMOVE`-cleanup statement) manages import-only artifacts this importer
//! already strips, so it is ignored **losslessly** and does not affect
//! `zero_loss`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::neo4j::{LabelStrategy, Neo4jFidelityReport, file_label_for, provenance_for};
use super::parse::{self, Row};
use super::{
    EdgePrepError, Endpoint, Importer, PreparedEdge, PreparedNode, RowError, UnresolvedEndpoint,
};
use crate::core::error::{Error, Result};
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
use crate::core::temporal::Timestamp;

/// Options controlling a Neo4j APOC Cypher-dump import (Issue #3356).
///
/// Mirrors the relevant flags of [`Neo4jCsvOptions`](super::Neo4jCsvOptions):
/// multi-label collapse strategy, opt-in vector properties, an optional
/// valid-from property, and strict-type handling — plus the apoc-specific key
/// scheme (`UNIQUE IMPORT ID` / `UNIQUE IMPORT LABEL`) and a fallback label for
/// label-less nodes.
#[derive(Debug, Clone)]
pub struct Neo4jCypherOptions {
    /// How multi-label nodes collapse onto AletheiaDB's single-label model.
    pub label_strategy: LabelStrategy,
    /// Property key under which the full label set is preserved (default
    /// `"_labels"`).
    pub retained_labels_key: String,
    /// Property names whose numeric list values become a dense
    /// [`PropertyValue::Vector`] instead of an [`PropertyValue::Array`].
    /// Explicit opt-in only (AC2).
    pub vector_properties: HashSet<String>,
    /// Optional property name whose temporal value designates each entity's
    /// `valid_from` (Issue #3221). Consumed as valid time, not stored.
    pub valid_from_property: Option<String>,
    /// When `true`, an unsupported value (`point(..)`, `duration(..)`, an
    /// unknown constructor) is a hard error instead of a reported-and-counted
    /// drop.
    pub strict_types: bool,
    /// The property apoc uses as the stable import identifier that
    /// relationships match on (default `"UNIQUE IMPORT ID"`). Its value becomes
    /// the node's business key and is **not** stored as a property.
    pub key_property: String,
    /// apoc's synthetic label applied to every exported node (default
    /// `"UNIQUE IMPORT LABEL"`). Stripped from the stored label set.
    pub unique_import_label: String,
    /// Label assigned to a node that has no Neo4j labels once the synthetic
    /// `unique_import_label` is stripped (default `"Node"`).
    pub default_label: String,
}

impl Default for Neo4jCypherOptions {
    fn default() -> Self {
        Neo4jCypherOptions {
            label_strategy: LabelStrategy::First,
            retained_labels_key: "_labels".to_string(),
            vector_properties: HashSet::new(),
            valid_from_property: None,
            strict_types: false,
            key_property: "UNIQUE IMPORT ID".to_string(),
            unique_import_label: "UNIQUE IMPORT LABEL".to_string(),
            default_label: "Node".to_string(),
        }
    }
}

impl Neo4jCypherOptions {
    /// Create default APOC Cypher-dump import options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the multi-label collapse strategy.
    #[must_use]
    pub fn label_strategy(mut self, strategy: LabelStrategy) -> Self {
        self.label_strategy = strategy;
        self
    }

    /// Set the property key used to preserve the full label set.
    #[must_use]
    pub fn retained_labels_key(mut self, key: impl Into<String>) -> Self {
        self.retained_labels_key = key.into();
        self
    }

    /// Mark a numeric-list property to be stored as a dense vector (AC2).
    #[must_use]
    pub fn vector_property(mut self, name: impl Into<String>) -> Self {
        self.vector_properties.insert(name.into());
        self
    }

    /// Designate a temporal property as each entity's `valid_from`.
    #[must_use]
    pub fn valid_from_property(mut self, name: impl Into<String>) -> Self {
        self.valid_from_property = Some(name.into());
        self
    }

    /// Turn an unsupported value into a hard error.
    #[must_use]
    pub fn strict_types(mut self, strict: bool) -> Self {
        self.strict_types = strict;
        self
    }

    /// Override the property relationships match on (apoc: `UNIQUE IMPORT ID`).
    #[must_use]
    pub fn key_property(mut self, key: impl Into<String>) -> Self {
        self.key_property = key.into();
        self
    }

    /// Override apoc's synthetic per-node label (apoc: `UNIQUE IMPORT LABEL`).
    #[must_use]
    pub fn unique_import_label(mut self, label: impl Into<String>) -> Self {
        self.unique_import_label = label.into();
        self
    }

    /// Override the fallback label for label-less nodes (default `Node`).
    #[must_use]
    pub fn default_label(mut self, label: impl Into<String>) -> Self {
        self.default_label = label.into();
        self
    }
}

// ---- Fidelity notes --------------------------------------------------------

/// Per-statement fidelity scratch, folded into the [`Neo4jFidelityReport`] only
/// once the produced node/edge is known to import (so a skipped or unresolved
/// entity never inflates the mapping / coercion / unsupported counts).
#[derive(Default, Clone)]
struct FidelityNote {
    label: Option<(String, String)>,
    edge_type: Option<String>,
    coercions: Vec<(String, &'static str)>,
    unsupported: Vec<(String, String)>,
    prop_count: usize,
}

impl FidelityNote {
    fn note_coercion(&mut self, column: &str, kind: &'static str) {
        if !self
            .coercions
            .iter()
            .any(|(c, k)| c == column && *k == kind)
        {
            self.coercions.push((column.to_string(), kind));
        }
    }

    fn note_unsupported(&mut self, column: &str, ty: &str) {
        if !self.unsupported.iter().any(|(c, _)| c == column) {
            self.unsupported.push((column.to_string(), ty.to_string()));
        }
    }
}

/// Fold a committed entity's [`FidelityNote`] into the shared report.
fn fold_note(report: &mut Neo4jFidelityReport, note: &FidelityNote) {
    if let Some((neo4j_labels, chosen)) = &note.label {
        report.note_label(neo4j_labels, chosen);
    }
    if let Some(edge_type) = &note.edge_type {
        report.note_type(edge_type);
    }
    for (column, kind) in &note.coercions {
        report.note_coercion(column, kind, super::neo4j::coercion_detail(kind));
    }
    for (column, ty) in &note.unsupported {
        report.note_unsupported(column, ty);
    }
    report.properties_imported += note.prop_count;
}

// ---- Parsed intermediate units ---------------------------------------------

/// A fully-parsed node ready to feed the node-import spine, plus its fidelity note.
struct NodeUnit {
    node: PreparedNode,
    note: FidelityNote,
}

/// A parsed relationship with unresolved endpoint keys, resolved in the
/// edge-import spine against the business-key map.
struct EdgeUnit {
    source_key: String,
    target_key: String,
    label: String,
    properties: PropertyMap,
    valid_from: Option<Timestamp>,
    note: FidelityNote,
    line: usize,
}

// ---- Statement splitting ---------------------------------------------------

/// One top-level statement of the dump, with the 1-based source line it began on.
struct Statement {
    line: usize,
    text: String,
}

/// Split an apoc Cypher dump into top-level statements.
///
/// A statement is either a `:`-prefixed meta command (`:begin` / `:commit` /
/// `:rollback`, read to end of line) or a `;`-terminated Cypher statement.
/// Semicolons inside strings, backtick identifiers, or bracket/brace/paren
/// nesting do not terminate a statement. A non-whitespace remainder with no
/// terminating `;` is a **truncated** dump and errors (never silently dropped).
fn split_statements(src: &str) -> std::result::Result<Vec<Statement>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;

    // Advance over whitespace, counting newlines; returns at first non-ws char.
    while i < chars.len() {
        // Skip leading whitespace.
        while i < chars.len() && chars[i].is_whitespace() {
            if chars[i] == '\n' {
                line += 1;
            }
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start_line = line;
        if chars[i] == ':' {
            // Meta command: read to end of line.
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            out.push(Statement {
                line: start_line,
                text: text.trim().to_string(),
            });
            continue;
        }

        // Cypher statement: accumulate to a top-level ';'.
        let start = i;
        let mut in_string: Option<char> = None;
        let mut in_backtick = false;
        let mut depth: i32 = 0;
        let mut terminated = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' {
                line += 1;
            }
            if let Some(q) = in_string {
                if c == '\\' {
                    // Skip the escaped char (count its newline if any).
                    i += 1;
                    if i < chars.len() && chars[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                    continue;
                }
                if c == q {
                    in_string = None;
                }
                i += 1;
                continue;
            }
            if in_backtick {
                if c == '`' {
                    in_backtick = false;
                }
                i += 1;
                continue;
            }
            match c {
                '\'' | '"' => in_string = Some(c),
                '`' => in_backtick = true,
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ';' if depth <= 0 => {
                    let text: String = chars[start..i].iter().collect();
                    out.push(Statement {
                        line: start_line,
                        text: text.trim().to_string(),
                    });
                    i += 1; // consume ';'
                    terminated = true;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        if !terminated {
            // Reached EOF mid-statement. If there is any non-whitespace, the dump
            // is truncated.
            let rem: String = chars[start..i].iter().collect();
            if !rem.trim().is_empty() {
                return Err(format!(
                    "truncated Cypher dump: statement beginning at line {start_line} \
                     has no terminating ';'"
                ));
            }
        }
    }
    Ok(out)
}

// ---- Statement classification ----------------------------------------------

/// The kind of a top-level Cypher statement.
enum StmtClass {
    /// A transaction marker (`:begin`/`:commit`/`:rollback`) or apoc scaffolding
    /// — ignored losslessly.
    Ignore,
    /// A node `CREATE`.
    Node,
    /// A relationship `MATCH ... CREATE`.
    Edge,
    /// An `UNWIND ... CREATE` batch (node or relationship, resolved on parse).
    Unwind,
    /// A supported-out-of-scope construct, reported as unsupported with a count.
    Unsupported { label: String, kind: String },
    /// An unrecognized statement — surfaced as a parse error per the failure mode.
    Unknown,
}

/// Case-insensitive first keyword of a statement (uppercased), or `""`.
fn leading_keyword(text: &str) -> String {
    text.trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// True if `kw` appears as a whole word (case-insensitive) outside strings and
/// backtick identifiers.
fn contains_keyword(text: &str, kw: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let kw_lower = kw.to_ascii_lowercase();
    let mut in_string: Option<char> = None;
    let mut in_backtick = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = in_string {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if in_backtick {
            if c == '`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => {
                in_string = Some(c);
                i += 1;
            }
            '`' => {
                in_backtick = true;
                i += 1;
            }
            _ if is_ident_start(c) => {
                let start = i;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if word.to_ascii_lowercase() == kw_lower {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

/// True if the statement contains a top-level relationship arrow (`-[`),
/// outside strings/backticks.
fn contains_rel_arrow(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut in_string: Option<char> = None;
    let mut in_backtick = false;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = in_string {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if in_backtick {
            if c == '`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => in_string = Some(c),
            '`' => in_backtick = true,
            '-' if i + 1 < chars.len() && chars[i + 1] == '[' => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn classify(text: &str) -> StmtClass {
    if text.is_empty() {
        return StmtClass::Ignore;
    }
    if text.starts_with(':') {
        // :begin / :commit / :rollback (and any other psql-style meta command).
        return StmtClass::Ignore;
    }
    let kw = leading_keyword(text);
    let lower = text.to_ascii_lowercase();
    let has_unique_import = lower.contains("unique import");

    match kw.as_str() {
        "CREATE" => {
            if contains_keyword(text, "CONSTRAINT") {
                if has_unique_import {
                    return StmtClass::Ignore; // apoc scaffolding
                }
                return StmtClass::Unsupported {
                    label: "CREATE CONSTRAINT".to_string(),
                    kind: "constraint".to_string(),
                };
            }
            if contains_keyword(text, "INDEX") {
                if has_unique_import {
                    return StmtClass::Ignore;
                }
                return StmtClass::Unsupported {
                    label: "CREATE INDEX".to_string(),
                    kind: "index".to_string(),
                };
            }
            if contains_rel_arrow(text) {
                // Inline `CREATE (a)-[:R]->(b)` (no MATCH) is not apoc's rel
                // format; report rather than guess node identities.
                return StmtClass::Unsupported {
                    label: "CREATE (inline relationship)".to_string(),
                    kind: "inline_relationship".to_string(),
                };
            }
            StmtClass::Node
        }
        "MATCH" => {
            if contains_keyword(text, "CREATE") {
                StmtClass::Edge
            } else if has_unique_import {
                // apoc's `MATCH (n:`UNIQUE IMPORT LABEL`) ... REMOVE ...` cleanup.
                StmtClass::Ignore
            } else {
                StmtClass::Unsupported {
                    label: "MATCH (mutation)".to_string(),
                    kind: "match_mutation".to_string(),
                }
            }
        }
        "UNWIND" => StmtClass::Unwind,
        "DROP" => {
            if has_unique_import {
                StmtClass::Ignore
            } else {
                StmtClass::Unsupported {
                    label: "DROP".to_string(),
                    kind: "drop".to_string(),
                }
            }
        }
        "CALL" => {
            if lower.contains("awaitindex") || lower.contains("await index") {
                StmtClass::Ignore // apoc index-await scaffolding
            } else {
                StmtClass::Unsupported {
                    label: "CALL".to_string(),
                    kind: "procedure_call".to_string(),
                }
            }
        }
        "SCHEMA" => StmtClass::Ignore, // `SCHEMA AWAIT`
        "MERGE" => StmtClass::Unsupported {
            label: "MERGE".to_string(),
            kind: "merge".to_string(),
        },
        _ => StmtClass::Unknown,
    }
}

// ---- Cursor + lexical helpers ----------------------------------------------

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// A char cursor over one statement's text.
struct Cursor {
    s: Vec<char>,
    pos: usize,
}

impl Cursor {
    fn new(text: &str) -> Self {
        Cursor {
            s: text.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.s.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn eof(&self) -> bool {
        self.pos >= self.s.len()
    }

    fn expect(&mut self, c: char) -> std::result::Result<(), String> {
        self.skip_ws();
        match self.peek() {
            Some(got) if got == c => {
                self.pos += 1;
                Ok(())
            }
            Some(got) => Err(format!("expected '{c}', found '{got}'")),
            None => Err(format!("expected '{c}', found end of statement")),
        }
    }

    /// Consume `kw` (case-insensitive) if it is the next whole word.
    fn consume_keyword(&mut self, kw: &str) -> std::result::Result<(), String> {
        self.skip_ws();
        let kw_chars: Vec<char> = kw.chars().collect();
        if self.pos + kw_chars.len() > self.s.len() {
            return Err(format!("expected keyword '{kw}'"));
        }
        for (k, kc) in kw_chars.iter().enumerate() {
            if !self.s[self.pos + k].eq_ignore_ascii_case(kc) {
                return Err(format!("expected keyword '{kw}'"));
            }
        }
        // Must be a word boundary.
        let after = self.pos + kw_chars.len();
        if let Some(&next) = self.s.get(after)
            && is_ident_continue(next)
        {
            return Err(format!("expected keyword '{kw}'"));
        }
        self.pos = after;
        Ok(())
    }

    /// Parse a bare or backtick-quoted identifier.
    fn parse_ident(&mut self) -> std::result::Result<String, String> {
        self.skip_ws();
        match self.peek() {
            Some('`') => {
                self.pos += 1;
                let mut out = String::new();
                loop {
                    match self.bump() {
                        None => return Err("unterminated backtick identifier".to_string()),
                        Some('`') => {
                            // Doubled backtick escapes a literal backtick.
                            if self.peek() == Some('`') {
                                out.push('`');
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                        Some(c) => out.push(c),
                    }
                }
                Ok(out)
            }
            Some(c) if is_ident_start(c) => {
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if is_ident_continue(c) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Ok(self.s[start..self.pos].iter().collect())
            }
            Some(c) => Err(format!("expected identifier, found '{c}'")),
            None => Err("expected identifier, found end of statement".to_string()),
        }
    }
}

// ---- Value parsing ---------------------------------------------------------

/// The outcome of parsing one property value.
enum PropOutcome {
    /// A stored value.
    Value(PropertyValue),
    /// `null` or an unsupported value that was dropped (and already noted).
    Absent,
}

/// Parse a Cypher string literal (the opening quote at the cursor).
fn parse_string(cur: &mut Cursor) -> std::result::Result<String, String> {
    let quote = cur.bump().ok_or("expected string quote")?;
    let mut out = String::new();
    loop {
        match cur.bump() {
            None => return Err("unterminated string literal".to_string()),
            Some('\\') => {
                let esc = cur.bump().ok_or("unterminated escape in string literal")?;
                match esc {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    '\\' => out.push('\\'),
                    '\'' => out.push('\''),
                    '"' => out.push('"'),
                    '/' => out.push('/'),
                    '`' => out.push('`'),
                    'u' => {
                        let mut hex = String::new();
                        for _ in 0..4 {
                            let h = cur.bump().ok_or("truncated \\u escape in string literal")?;
                            hex.push(h);
                        }
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|_| format!("invalid \\u escape '\\u{hex}'"))?;
                        let ch = char::from_u32(code)
                            .ok_or_else(|| format!("invalid unicode code point U+{hex}"))?;
                        out.push(ch);
                    }
                    other => return Err(format!("invalid escape '\\{other}' in string literal")),
                }
            }
            Some(c) if c == quote => break,
            Some(c) => out.push(c),
        }
    }
    Ok(out)
}

/// Parse a numeric literal at the cursor into an `Int` or `Float` value.
fn parse_number(cur: &mut Cursor) -> std::result::Result<PropertyValue, String> {
    let start = cur.pos;
    if cur.peek() == Some('-') || cur.peek() == Some('+') {
        cur.pos += 1;
    }
    let mut is_float = false;
    let mut saw_digit = false;
    while let Some(c) = cur.peek() {
        if c.is_ascii_digit() {
            saw_digit = true;
            cur.pos += 1;
        } else {
            break;
        }
    }
    if cur.peek() == Some('.') {
        is_float = true;
        cur.pos += 1;
        while let Some(c) = cur.peek() {
            if c.is_ascii_digit() {
                saw_digit = true;
                cur.pos += 1;
            } else {
                break;
            }
        }
    }
    if matches!(cur.peek(), Some('e') | Some('E')) {
        is_float = true;
        cur.pos += 1;
        if matches!(cur.peek(), Some('+') | Some('-')) {
            cur.pos += 1;
        }
        while let Some(c) = cur.peek() {
            if c.is_ascii_digit() {
                cur.pos += 1;
            } else {
                break;
            }
        }
    }
    if !saw_digit {
        return Err("malformed numeric literal".to_string());
    }
    let text: String = cur.s[start..cur.pos].iter().collect();
    if is_float {
        text.parse::<f64>()
            .map(PropertyValue::Float)
            .map_err(|_| format!("malformed float literal '{text}'"))
    } else {
        match text.parse::<i64>() {
            Ok(v) => Ok(PropertyValue::Int(v)),
            // An integer that overflows i64 promotes to Float rather than failing.
            Err(_) => text
                .parse::<f64>()
                .map(PropertyValue::Float)
                .map_err(|_| format!("malformed integer literal '{text}'")),
        }
    }
}

/// Consume a balanced parenthesized group (the opening `(` already consumed),
/// respecting nested brackets and string literals. Used to skip over an
/// unsupported constructor's arguments.
fn skip_balanced_parens(cur: &mut Cursor) -> std::result::Result<(), String> {
    let mut depth = 1i32;
    while let Some(c) = cur.bump() {
        match c {
            '\'' | '"' => {
                // Skip a string literal.
                let quote = c;
                loop {
                    match cur.bump() {
                        None => return Err("unterminated string in arguments".to_string()),
                        Some('\\') => {
                            cur.bump();
                        }
                        Some(x) if x == quote => break,
                        _ => {}
                    }
                }
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    Err("unbalanced parentheses in arguments".to_string())
}

/// Parse a temporal / unsupported constructor call. The identifier `name` has
/// been read; the cursor is at the `(`.
fn parse_function(
    cur: &mut Cursor,
    name: &str,
    prop_name: &str,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<PropOutcome, String> {
    let lower = name.to_ascii_lowercase();
    cur.expect('(')?;
    match lower.as_str() {
        "datetime" | "localdatetime" | "date" | "zoneddatetime" => {
            cur.skip_ws();
            let arg = match cur.peek() {
                Some('\'') | Some('"') => parse_string(cur)?,
                _ => {
                    skip_balanced_parens(cur)?;
                    return unsupported_value(prop_name, &lower, opts, note);
                }
            };
            cur.expect(')')?;
            let ts = parse::parse_valid_time(strip_zone_id(arg.trim()))
                .map_err(|m| format!("temporal value '{arg}': {m}"))?;
            note.note_coercion(prop_name, "temporal_to_micros");
            Ok(PropOutcome::Value(PropertyValue::Int(ts.wallclock())))
        }
        "time" | "localtime" => {
            cur.skip_ws();
            let arg = match cur.peek() {
                Some('\'') | Some('"') => parse_string(cur)?,
                _ => {
                    skip_balanced_parens(cur)?;
                    return unsupported_value(prop_name, &lower, opts, note);
                }
            };
            cur.expect(')')?;
            note.note_coercion(prop_name, "time_to_string");
            Ok(PropOutcome::Value(PropertyValue::string(arg)))
        }
        _ => {
            // point(..), duration(..), or any unknown constructor / byte-array
            // encoding: rejected + reported (never silently dropped).
            skip_balanced_parens(cur)?;
            unsupported_value(prop_name, &lower, opts, note)
        }
    }
}

/// Record (or, under strict mode, hard-error on) an unsupported value.
fn unsupported_value(
    prop_name: &str,
    ty: &str,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<PropOutcome, String> {
    if opts.strict_types {
        return Err(format!(
            "unsupported value type '{ty}' for property '{prop_name}' (strict-types)"
        ));
    }
    note.note_unsupported(prop_name, ty);
    Ok(PropOutcome::Absent)
}

/// Strip a trailing bracketed IANA zone id (`...+01:00[Europe/London]`).
fn strip_zone_id(s: &str) -> &str {
    if s.ends_with(']')
        && let Some(open) = s.rfind('[')
    {
        return s[..open].trim_end();
    }
    s
}

/// Parse one property value.
fn parse_value(
    cur: &mut Cursor,
    prop_name: &str,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<PropOutcome, String> {
    cur.skip_ws();
    match cur.peek() {
        None => Err("unexpected end of value".to_string()),
        Some('\'') | Some('"') => Ok(PropOutcome::Value(PropertyValue::string(parse_string(
            cur,
        )?))),
        Some('[') => parse_list_value(cur, prop_name, opts),
        Some('{') => {
            let json = parse_json_value(cur, opts)?;
            note.note_coercion(prop_name, "map_to_json");
            Ok(PropOutcome::Value(PropertyValue::string(json.to_string())))
        }
        Some(c) if c == '-' || c == '+' || c.is_ascii_digit() => {
            Ok(PropOutcome::Value(parse_number(cur)?))
        }
        Some(c) if is_ident_start(c) || c == '`' => {
            let ident = cur.parse_ident()?;
            cur.skip_ws();
            if cur.peek() == Some('(') {
                return parse_function(cur, &ident, prop_name, opts, note);
            }
            match ident.to_ascii_lowercase().as_str() {
                "true" => Ok(PropOutcome::Value(PropertyValue::Bool(true))),
                "false" => Ok(PropOutcome::Value(PropertyValue::Bool(false))),
                "null" => Ok(PropOutcome::Absent),
                other => Err(format!("unexpected bare literal '{other}' in value")),
            }
        }
        Some(c) => Err(format!("unexpected character '{c}' in value")),
    }
}

/// Parse a `[..]` list, applying vector detection for opted-in properties.
fn parse_list_value(
    cur: &mut Cursor,
    prop_name: &str,
    opts: &Neo4jCypherOptions,
) -> std::result::Result<PropOutcome, String> {
    cur.expect('[')?;
    let mut elems: Vec<serde_json::Value> = Vec::new();
    cur.skip_ws();
    if cur.peek() == Some(']') {
        cur.pos += 1;
    } else {
        loop {
            let v = parse_json_value(cur, opts)?;
            elems.push(v);
            cur.skip_ws();
            match cur.peek() {
                Some(',') => {
                    cur.pos += 1;
                }
                Some(']') => {
                    cur.pos += 1;
                    break;
                }
                Some(c) => return Err(format!("expected ',' or ']' in list, found '{c}'")),
                None => return Err("unterminated list".to_string()),
            }
        }
    }

    let all_numeric =
        !elems.is_empty() && elems.iter().all(|v| v.is_number() && v.as_f64().is_some());
    if opts.vector_properties.contains(prop_name) && all_numeric {
        let floats: Vec<f32> = elems
            .iter()
            .filter_map(|v| v.as_f64())
            .map(|f| f as f32)
            .collect();
        let vec = PropertyValue::try_vector(&floats).map_err(|e| e.to_string())?;
        return Ok(PropOutcome::Value(vec));
    }
    let values: Vec<PropertyValue> = elems.iter().filter_map(json_to_property_value).collect();
    Ok(PropOutcome::Value(PropertyValue::array(values)))
}

/// Parse a Cypher value into a `serde_json::Value` (for nested maps, list
/// elements, and UNWIND rows). Temporal constructors keep their string
/// argument; unsupported constructors become JSON `null`.
fn parse_json_value(
    cur: &mut Cursor,
    opts: &Neo4jCypherOptions,
) -> std::result::Result<serde_json::Value, String> {
    cur.skip_ws();
    match cur.peek() {
        None => Err("unexpected end of value".to_string()),
        Some('\'') | Some('"') => Ok(serde_json::Value::String(parse_string(cur)?)),
        Some('{') => parse_json_map(cur, opts),
        Some('[') => {
            cur.pos += 1;
            let mut arr = Vec::new();
            cur.skip_ws();
            if cur.peek() == Some(']') {
                cur.pos += 1;
                return Ok(serde_json::Value::Array(arr));
            }
            loop {
                arr.push(parse_json_value(cur, opts)?);
                cur.skip_ws();
                match cur.peek() {
                    Some(',') => cur.pos += 1,
                    Some(']') => {
                        cur.pos += 1;
                        break;
                    }
                    Some(c) => return Err(format!("expected ',' or ']', found '{c}'")),
                    None => return Err("unterminated list".to_string()),
                }
            }
            Ok(serde_json::Value::Array(arr))
        }
        Some(c) if c == '-' || c == '+' || c.is_ascii_digit() => match parse_number(cur)? {
            PropertyValue::Int(i) => Ok(serde_json::Value::from(i)),
            PropertyValue::Float(f) => Ok(serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)),
            _ => Ok(serde_json::Value::Null),
        },
        Some(c) if is_ident_start(c) || c == '`' => {
            let ident = cur.parse_ident()?;
            cur.skip_ws();
            if cur.peek() == Some('(') {
                let lower = ident.to_ascii_lowercase();
                cur.expect('(')?;
                match lower.as_str() {
                    "datetime" | "localdatetime" | "date" | "zoneddatetime" | "time"
                    | "localtime" => {
                        cur.skip_ws();
                        let arg = match cur.peek() {
                            Some('\'') | Some('"') => parse_string(cur)?,
                            _ => {
                                skip_balanced_parens(cur)?;
                                return Ok(serde_json::Value::Null);
                            }
                        };
                        cur.expect(')')?;
                        Ok(serde_json::Value::String(arg))
                    }
                    _ => {
                        skip_balanced_parens(cur)?;
                        Ok(serde_json::Value::Null)
                    }
                }
            } else {
                match ident.to_ascii_lowercase().as_str() {
                    "true" => Ok(serde_json::Value::Bool(true)),
                    "false" => Ok(serde_json::Value::Bool(false)),
                    "null" => Ok(serde_json::Value::Null),
                    other => Err(format!("unexpected bare literal '{other}'")),
                }
            }
        }
        Some(c) => Err(format!("unexpected character '{c}' in value")),
    }
}

/// Parse a `{ key: value, .. }` map into a `serde_json::Value::Object`.
fn parse_json_map(
    cur: &mut Cursor,
    opts: &Neo4jCypherOptions,
) -> std::result::Result<serde_json::Value, String> {
    cur.expect('{')?;
    let mut map = serde_json::Map::new();
    cur.skip_ws();
    if cur.peek() == Some('}') {
        cur.pos += 1;
        return Ok(serde_json::Value::Object(map));
    }
    loop {
        let key = cur.parse_ident()?;
        cur.expect(':')?;
        let val = parse_json_value(cur, opts)?;
        map.insert(key, val);
        cur.skip_ws();
        match cur.peek() {
            Some(',') => cur.pos += 1,
            Some('}') => {
                cur.pos += 1;
                break;
            }
            Some(c) => return Err(format!("expected ',' or '}}' in map, found '{c}'")),
            None => return Err("unterminated map".to_string()),
        }
    }
    Ok(serde_json::Value::Object(map))
}

/// Convert a `serde_json::Value` scalar/array to a `PropertyValue`
/// (`None` for JSON null). Nested objects/arrays become a JSON string / array.
fn json_to_property_value(v: &serde_json::Value) -> Option<PropertyValue> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(PropertyValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(PropertyValue::Int(i))
            } else {
                n.as_f64().map(PropertyValue::Float)
            }
        }
        serde_json::Value::String(s) => Some(PropertyValue::string(s)),
        serde_json::Value::Array(items) => Some(PropertyValue::array(
            items.iter().filter_map(json_to_property_value).collect(),
        )),
        serde_json::Value::Object(_) => Some(PropertyValue::string(v.to_string())),
    }
}

/// A property (as a JSON value) destined for the property bag, with vector
/// detection for opted-in names.
fn json_property_to_value(
    name: &str,
    v: &serde_json::Value,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> Option<PropertyValue> {
    if let serde_json::Value::Array(items) = v
        && opts.vector_properties.contains(name)
        && !items.is_empty()
        && items.iter().all(|e| e.is_number() && e.as_f64().is_some())
    {
        let floats: Vec<f32> = items
            .iter()
            .filter_map(|e| e.as_f64())
            .map(|f| f as f32)
            .collect();
        return PropertyValue::try_vector(&floats).ok();
    }
    if matches!(v, serde_json::Value::Object(_)) {
        note.note_coercion(name, "map_to_json");
    }
    json_to_property_value(v)
}

/// The canonical business-key string for a value (must match on both the node
/// key side and the relationship-match side).
fn canonical_key(v: &PropertyValue) -> String {
    match v {
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::String(s) => s.to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// The canonical business-key string for a JSON value (UNWIND path).
fn canonical_key_json(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ---- Pattern parsing -------------------------------------------------------

/// A parsed node pattern `( [var] (:Label)* {props}? )`.
struct NodePattern {
    var: Option<String>,
    labels: Vec<String>,
    props: Vec<(String, PropOutcome)>,
}

/// Parse a node pattern at the cursor (the `(` is the next non-ws char).
fn parse_node_pattern(
    cur: &mut Cursor,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<NodePattern, String> {
    cur.expect('(')?;
    cur.skip_ws();
    let mut var = None;
    // Optional variable: a bare identifier not introducing a label/prop/close.
    if let Some(c) = cur.peek()
        && c != ':'
        && c != '{'
        && c != ')'
    {
        var = Some(cur.parse_ident()?);
    }
    let mut labels = Vec::new();
    loop {
        cur.skip_ws();
        if cur.peek() == Some(':') {
            cur.pos += 1;
            labels.push(cur.parse_ident()?);
        } else {
            break;
        }
    }
    cur.skip_ws();
    let mut props = Vec::new();
    if cur.peek() == Some('{') {
        props = parse_prop_map(cur, opts, note)?;
    }
    cur.expect(')')?;
    Ok(NodePattern { var, labels, props })
}

/// Parse a `{ key: value, .. }` property bag for a node/relationship.
fn parse_prop_map(
    cur: &mut Cursor,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<Vec<(String, PropOutcome)>, String> {
    cur.expect('{')?;
    let mut out = Vec::new();
    cur.skip_ws();
    if cur.peek() == Some('}') {
        cur.pos += 1;
        return Ok(out);
    }
    loop {
        let key = cur.parse_ident()?;
        cur.expect(':')?;
        let val = parse_value(cur, &key, opts, note)?;
        out.push((key, val));
        cur.skip_ws();
        match cur.peek() {
            Some(',') => cur.pos += 1,
            Some('}') => {
                cur.pos += 1;
                break;
            }
            Some(c) => return Err(format!("expected ',' or '}}' in property map, found '{c}'")),
            None => return Err("unterminated property map".to_string()),
        }
    }
    Ok(out)
}

/// Build a [`PreparedNode`] (and finish its fidelity note) from a parsed node
/// pattern. `seq` disambiguates the synthetic key of a keyless node.
fn build_node(
    pattern: NodePattern,
    opts: &Neo4jCypherOptions,
    seq: usize,
) -> std::result::Result<NodeUnit, String> {
    let mut note = FidelityNote::default();

    let real_labels: Vec<String> = pattern
        .labels
        .iter()
        .filter(|l| *l != &opts.unique_import_label)
        .cloned()
        .collect();

    let (chosen_label, neo4j_labels) = if real_labels.is_empty() {
        note.note_coercion(":LABEL", "default_label");
        (opts.default_label.clone(), "(none)".to_string())
    } else {
        let chosen = match opts.label_strategy {
            LabelStrategy::Concat => real_labels.join("_"),
            LabelStrategy::First | LabelStrategy::Property => real_labels[0].clone(),
        };
        (chosen, real_labels.join(":"))
    };
    note.label = Some((neo4j_labels, chosen_label.clone()));
    if real_labels.len() > 1 {
        note.note_coercion(":LABEL", "multi_label_flattened");
    }

    let mut builder = PropertyMapBuilder::new();
    let mut prop_count = 0usize;

    // Preserve the full label set as `_labels` (First: only when multi-label;
    // Property: always; Concat: never).
    let store_labels = match opts.label_strategy {
        LabelStrategy::First => real_labels.len() > 1,
        LabelStrategy::Property => !real_labels.is_empty(),
        LabelStrategy::Concat => false,
    };
    if store_labels {
        let arr = PropertyValue::array(real_labels.iter().map(PropertyValue::string).collect());
        builder = builder
            .try_insert(&opts.retained_labels_key, arr)
            .map_err(|e| e.to_string())?;
        prop_count += 1;
    }

    let mut key: Option<String> = None;
    let mut valid_from: Option<Timestamp> = None;
    for (name, outcome) in pattern.props {
        if name == opts.key_property {
            if let PropOutcome::Value(v) = &outcome {
                key = Some(canonical_key(v));
            }
            continue; // consumed as the business key, not stored
        }
        if opts.valid_from_property.as_deref() == Some(name.as_str()) {
            valid_from = extract_valid_from(&outcome, &name)?;
            continue;
        }
        if let PropOutcome::Value(v) = outcome {
            builder = builder.try_insert(&name, v).map_err(|e| e.to_string())?;
            prop_count += 1;
        }
    }

    note.prop_count = prop_count;
    let key = key.unwrap_or_else(|| format!("\0__no_key_{seq}"));
    Ok(NodeUnit {
        node: PreparedNode {
            key,
            label: chosen_label,
            properties: builder.build(),
            valid_from,
        },
        note,
    })
}

/// Extract a `valid_from` timestamp from a designated property's value.
fn extract_valid_from(
    outcome: &PropOutcome,
    name: &str,
) -> std::result::Result<Option<Timestamp>, String> {
    match outcome {
        PropOutcome::Value(PropertyValue::Int(micros)) => Ok(Some(Timestamp::from(*micros))),
        PropOutcome::Value(PropertyValue::String(s)) => parse::parse_valid_time(s)
            .map(Some)
            .map_err(|m| format!("valid-from property '{name}': {m}")),
        PropOutcome::Absent => Ok(None),
        PropOutcome::Value(other) => Err(format!(
            "valid-from property '{name}' is not a temporal value ({})",
            other.type_name()
        )),
    }
}

/// Resolve the business key referenced by a relationship-match property bag:
/// the configured `key_property` if present, else the sole property.
fn match_key(
    props: &[(String, PropOutcome)],
    opts: &Neo4jCypherOptions,
) -> std::result::Result<String, String> {
    if let Some((_, PropOutcome::Value(v))) = props.iter().find(|(k, _)| k == &opts.key_property) {
        return Ok(canonical_key(v));
    }
    if props.len() == 1
        && let (_, PropOutcome::Value(v)) = &props[0]
    {
        return Ok(canonical_key(v));
    }
    Err(format!(
        "cannot determine endpoint key: no '{}' property and not exactly one match property",
        opts.key_property
    ))
}

/// Parse a `MATCH (a {..}), (b {..}) CREATE (a)-[:TYPE {..}]->(b)` statement.
fn parse_edge(
    text: &str,
    line: usize,
    opts: &Neo4jCypherOptions,
) -> std::result::Result<EdgeUnit, String> {
    let mut cur = Cursor::new(text);
    let mut note = FidelityNote::default();

    // MATCH clauses: one or more node patterns, separated by ',' or repeated
    // MATCH keywords.
    let mut match_vars: HashMap<String, Vec<(String, PropOutcome)>> = HashMap::new();
    cur.consume_keyword("MATCH")?;
    loop {
        let pat = parse_node_pattern(&mut cur, opts, &mut note)?;
        if let Some(var) = pat.var {
            match_vars.insert(var, pat.props);
        }
        cur.skip_ws();
        if cur.peek() == Some(',') {
            cur.pos += 1;
            continue;
        }
        if cur.consume_keyword("MATCH").is_ok() {
            continue;
        }
        break;
    }

    cur.consume_keyword("CREATE")?;
    // Relationship path: (a) <-/- [ rel ] -/-> (b)
    cur.expect('(')?;
    let a_var = cur.parse_ident()?;
    cur.expect(')')?;
    cur.skip_ws();
    let left = cur.peek() == Some('<');
    if left {
        cur.pos += 1;
    }
    cur.expect('-')?;
    cur.expect('[')?;
    // Optional relationship variable before the ':'.
    cur.skip_ws();
    if let Some(c) = cur.peek()
        && c != ':'
    {
        let _relvar = cur.parse_ident()?;
    }
    cur.expect(':')?;
    let rel_type = cur.parse_ident()?;
    cur.skip_ws();
    let mut rel_props = Vec::new();
    if cur.peek() == Some('{') {
        rel_props = parse_prop_map(&mut cur, opts, &mut note)?;
    }
    cur.expect(']')?;
    cur.expect('-')?;
    cur.skip_ws();
    let right = cur.peek() == Some('>');
    if right {
        cur.pos += 1;
    }
    cur.expect('(')?;
    let b_var = cur.parse_ident()?;
    cur.expect(')')?;

    // Direction: default a -> b; `<-...-` reverses.
    let (src_var, dst_var) = if left && !right {
        (b_var, a_var)
    } else {
        (a_var, b_var)
    };

    let src_props = match_vars
        .get(&src_var)
        .ok_or_else(|| format!("relationship references unmatched variable '{src_var}'"))?;
    let dst_props = match_vars
        .get(&dst_var)
        .ok_or_else(|| format!("relationship references unmatched variable '{dst_var}'"))?;
    let source_key = match_key(src_props, opts)?;
    let target_key = match_key(dst_props, opts)?;

    note.edge_type = Some(rel_type.clone());

    // Build the relationship property map.
    let mut builder = PropertyMapBuilder::new();
    let mut prop_count = 0usize;
    let mut valid_from = None;
    for (name, outcome) in rel_props {
        if opts.valid_from_property.as_deref() == Some(name.as_str()) {
            valid_from = extract_valid_from(&outcome, &name)?;
            continue;
        }
        if let PropOutcome::Value(v) = outcome {
            builder = builder.try_insert(&name, v).map_err(|e| e.to_string())?;
            prop_count += 1;
        }
    }
    note.prop_count = prop_count;

    Ok(EdgeUnit {
        source_key,
        target_key,
        label: rel_type,
        properties: builder.build(),
        valid_from,
        note,
        line,
    })
}

// ---- UNWIND batch parsing --------------------------------------------------

/// Parse an `UNWIND [ .. ] AS row ...` batch into node and/or relationship units.
fn parse_unwind(
    text: &str,
    line: usize,
    opts: &Neo4jCypherOptions,
    seq_start: usize,
) -> std::result::Result<(Vec<NodeUnit>, Vec<EdgeUnit>), String> {
    let mut cur = Cursor::new(text);
    cur.consume_keyword("UNWIND")?;
    cur.skip_ws();
    let rows = parse_json_value(&mut cur, opts)?;
    let serde_json::Value::Array(rows) = rows else {
        return Err("UNWIND expects a list literal".to_string());
    };
    cur.consume_keyword("AS")?;
    let _row_var = cur.parse_ident()?;

    // Remaining clause decides node vs relationship batch.
    let remainder: String = cur.s[cur.pos..].iter().collect();
    let is_rel = contains_keyword(&remainder, "MATCH");

    if is_rel {
        let rel_type = extract_unwind_rel_type(&remainder)?;
        let mut edges = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let obj = row
                .as_object()
                .ok_or_else(|| format!("UNWIND row {i} is not a map"))?;
            let start_id = obj
                .get("start")
                .and_then(|s| s.get("_id"))
                .and_then(canonical_key_json)
                .ok_or_else(|| format!("UNWIND relationship row {i} lacks start._id"))?;
            let end_id = obj
                .get("end")
                .and_then(|e| e.get("_id"))
                .and_then(canonical_key_json)
                .ok_or_else(|| format!("UNWIND relationship row {i} lacks end._id"))?;
            let mut note = FidelityNote {
                edge_type: Some(rel_type.clone()),
                ..Default::default()
            };
            let (properties, valid_from, prop_count) =
                unwind_properties(obj.get("properties"), opts, &mut note)?;
            note.prop_count = prop_count;
            edges.push(EdgeUnit {
                source_key: start_id,
                target_key: end_id,
                label: rel_type.clone(),
                properties,
                valid_from,
                note,
                line,
            });
        }
        Ok((Vec::new(), edges))
    } else {
        let labels = extract_unwind_node_labels(&remainder)?;
        let real_labels: Vec<String> = labels
            .iter()
            .filter(|l| *l != &opts.unique_import_label)
            .cloned()
            .collect();
        let mut nodes = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let obj = row
                .as_object()
                .ok_or_else(|| format!("UNWIND row {i} is not a map"))?;
            let key = obj
                .get("_id")
                .and_then(canonical_key_json)
                .unwrap_or_else(|| format!("\0__no_key_{}", seq_start + i));
            let unit = build_unwind_node(&real_labels, obj.get("properties"), key, opts)?;
            nodes.push(unit);
        }
        Ok((nodes, Vec::new()))
    }
}

/// Extract the relationship TYPE from an UNWIND's trailing `CREATE (a)-[:TYPE]->(b)`.
fn extract_unwind_rel_type(remainder: &str) -> std::result::Result<String, String> {
    let mut cur = Cursor::new(remainder);
    // Skip to CREATE.
    while !cur.eof() {
        if cur.consume_keyword("CREATE").is_ok() {
            break;
        }
        cur.pos += 1;
    }
    if cur.eof() {
        return Err("UNWIND relationship batch has no CREATE".to_string());
    }
    cur.expect('(')?;
    let _a = cur.parse_ident()?;
    cur.expect(')')?;
    cur.skip_ws();
    if cur.peek() == Some('<') {
        cur.pos += 1;
    }
    cur.expect('-')?;
    cur.expect('[')?;
    cur.skip_ws();
    if let Some(c) = cur.peek()
        && c != ':'
    {
        let _relvar = cur.parse_ident()?;
    }
    cur.expect(':')?;
    cur.parse_ident()
}

/// Extract the node labels from an UNWIND's trailing `CREATE (n:Labels ..)`.
fn extract_unwind_node_labels(remainder: &str) -> std::result::Result<Vec<String>, String> {
    let mut cur = Cursor::new(remainder);
    while !cur.eof() {
        if cur.consume_keyword("CREATE").is_ok() {
            break;
        }
        cur.pos += 1;
    }
    if cur.eof() {
        return Err("UNWIND node batch has no CREATE".to_string());
    }
    cur.expect('(')?;
    cur.skip_ws();
    // Optional variable.
    if let Some(c) = cur.peek()
        && c != ':'
        && c != '{'
        && c != ')'
    {
        let _var = cur.parse_ident()?;
    }
    let mut labels = Vec::new();
    loop {
        cur.skip_ws();
        if cur.peek() == Some(':') {
            cur.pos += 1;
            labels.push(cur.parse_ident()?);
        } else {
            break;
        }
    }
    if labels.is_empty() {
        return Err("UNWIND node batch CREATE has no label".to_string());
    }
    Ok(labels)
}

/// Flatten an UNWIND row's `properties` object into a fresh property map
/// (relationship path).
fn unwind_properties(
    properties: Option<&serde_json::Value>,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<(PropertyMap, Option<Timestamp>, usize), String> {
    let (builder, valid_from, count) =
        insert_unwind_properties(PropertyMapBuilder::new(), properties, opts, note)?;
    Ok((builder.build(), valid_from, count))
}

/// Insert an UNWIND row's `properties` object into an existing builder,
/// consuming the designated `valid_from` property.
fn insert_unwind_properties(
    mut builder: PropertyMapBuilder,
    properties: Option<&serde_json::Value>,
    opts: &Neo4jCypherOptions,
    note: &mut FidelityNote,
) -> std::result::Result<(PropertyMapBuilder, Option<Timestamp>, usize), String> {
    let mut count = 0usize;
    let mut valid_from = None;
    if let Some(serde_json::Value::Object(map)) = properties {
        for (name, val) in map {
            if opts.valid_from_property.as_deref() == Some(name.as_str()) {
                valid_from = json_valid_from(val, name)?;
                continue;
            }
            if let Some(v) = json_property_to_value(name, val, opts, note) {
                builder = builder.try_insert(name, v).map_err(|e| e.to_string())?;
                count += 1;
            }
        }
    }
    Ok((builder, valid_from, count))
}

/// A node produced from one UNWIND row.
fn build_unwind_node(
    real_labels: &[String],
    properties: Option<&serde_json::Value>,
    key: String,
    opts: &Neo4jCypherOptions,
) -> std::result::Result<NodeUnit, String> {
    let mut note = FidelityNote::default();
    let (chosen_label, neo4j_labels) = if real_labels.is_empty() {
        note.note_coercion(":LABEL", "default_label");
        (opts.default_label.clone(), "(none)".to_string())
    } else {
        let chosen = match opts.label_strategy {
            LabelStrategy::Concat => real_labels.join("_"),
            LabelStrategy::First | LabelStrategy::Property => real_labels[0].clone(),
        };
        (chosen, real_labels.join(":"))
    };
    note.label = Some((neo4j_labels, chosen_label.clone()));
    if real_labels.len() > 1 {
        note.note_coercion(":LABEL", "multi_label_flattened");
    }

    let mut builder = PropertyMapBuilder::new();
    let store_labels = match opts.label_strategy {
        LabelStrategy::First => real_labels.len() > 1,
        LabelStrategy::Property => !real_labels.is_empty(),
        LabelStrategy::Concat => false,
    };
    let mut count = 0usize;
    if store_labels {
        let arr = PropertyValue::array(real_labels.iter().map(PropertyValue::string).collect());
        builder = builder
            .try_insert(&opts.retained_labels_key, arr)
            .map_err(|e| e.to_string())?;
        count += 1;
    }
    let (builder, valid_from, pc) = insert_unwind_properties(builder, properties, opts, &mut note)?;
    count += pc;
    note.prop_count = count;
    Ok(NodeUnit {
        node: PreparedNode {
            key,
            label: chosen_label,
            properties: builder.build(),
            valid_from,
        },
        note,
    })
}

/// Extract a `valid_from` timestamp from a JSON property value.
fn json_valid_from(
    val: &serde_json::Value,
    name: &str,
) -> std::result::Result<Option<Timestamp>, String> {
    match val {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) if n.as_i64().is_some() => {
            Ok(Some(Timestamp::from(n.as_i64().unwrap())))
        }
        serde_json::Value::String(s) => parse::parse_valid_time(s)
            .map(Some)
            .map_err(|m| format!("valid-from property '{name}': {m}")),
        other => Err(format!(
            "valid-from property '{name}' is not a temporal value ({other})"
        )),
    }
}

// ---- Public API ------------------------------------------------------------

impl Importer<'_> {
    /// Import a Neo4j **APOC Cypher-script dump** (`apoc.export.cypher.all`
    /// `create`-format output) from a single file (Issue #3356).
    ///
    /// Parses the whole dump, streams node `CREATE`s and relationship
    /// `MATCH ... CREATE`s through the shared chunked-commit spine (nodes first,
    /// so relationship endpoints resolve), stamps `neo4j-import::<file>`
    /// provenance on every version, and returns a machine-readable fidelity
    /// report (counts, label/type mapping tables, coercion and unsupported-
    /// construct notes, and a `zero_loss` flag).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read; the dump is truncated
    /// (a statement with no terminating `;`); or — in the default
    /// [`FailureMode::Abort`](super::FailureMode::Abort) — the first malformed
    /// statement, unsupported value under `strict_types`, or unresolved
    /// relationship endpoint. In
    /// [`SkipAndReport`](super::FailureMode::SkipAndReport) mode those
    /// per-statement failures are collected in the report instead, each carrying
    /// the source file and 1-based line number.
    pub fn neo4j_import_cypher(
        &mut self,
        path: impl AsRef<Path>,
        opts: &Neo4jCypherOptions,
    ) -> Result<Neo4jFidelityReport> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path)
            .map_err(|e| Error::other(format!("opening {}: {e}", path.display())))?;
        let file_label = file_label_for(path);

        let statements = split_statements(&src).map_err(Error::other)?;
        let mut report = Neo4jFidelityReport::default();

        // --- Parse phase: classify + parse every statement, honoring the
        // failure mode with per-statement line locations. ---
        let mut node_units: Vec<NodeUnit> = Vec::new();
        let mut edge_units: Vec<EdgeUnit> = Vec::new();
        let mut rows_read = 0usize;

        for stmt in statements {
            match classify(&stmt.text) {
                StmtClass::Ignore => {}
                StmtClass::Unsupported { label, kind } => {
                    report.note_unsupported(&label, &kind);
                }
                StmtClass::Node => {
                    rows_read += 1;
                    let seq = node_units.len();
                    match build_node_stmt(&stmt.text, opts, seq) {
                        Ok(unit) => node_units.push(unit),
                        Err(msg) => {
                            self.record_parse_failure(stmt.line, msg, &file_label, &mut report)?;
                        }
                    }
                }
                StmtClass::Edge => {
                    rows_read += 1;
                    match parse_edge(&stmt.text, stmt.line, opts) {
                        Ok(unit) => edge_units.push(unit),
                        Err(msg) => {
                            self.record_parse_failure(stmt.line, msg, &file_label, &mut report)?;
                        }
                    }
                }
                StmtClass::Unwind => {
                    let seq = node_units.len();
                    match parse_unwind(&stmt.text, stmt.line, opts, seq) {
                        Ok((mut ns, mut es)) => {
                            rows_read += ns.len() + es.len();
                            node_units.append(&mut ns);
                            edge_units.append(&mut es);
                        }
                        Err(msg) => {
                            rows_read += 1;
                            self.record_parse_failure(stmt.line, msg, &file_label, &mut report)?;
                        }
                    }
                }
                StmtClass::Unknown => {
                    rows_read += 1;
                    let kw = leading_keyword(&stmt.text);
                    self.record_parse_failure(
                        stmt.line,
                        format!("unrecognized statement (leading keyword '{kw}')"),
                        &file_label,
                        &mut report,
                    )?;
                }
            }
        }

        // --- Commit phase: nodes first (build the key map), then edges. ---
        self.commit_nodes(node_units, path, &mut report)?;
        self.commit_edges(edge_units, path, &file_label, &mut report)?;

        report.rows_read = rows_read;
        report.finalize();
        Ok(report)
    }

    /// Apply the failure mode to a per-statement parse failure.
    fn record_parse_failure(
        &self,
        line: usize,
        message: String,
        file_label: &str,
        report: &mut Neo4jFidelityReport,
    ) -> Result<()> {
        let row_err = RowError {
            row: line,
            message,
            file: Some(file_label.to_string()),
        };
        match self.config.failure_mode {
            super::FailureMode::Abort => Err(super::ImportError::Row(row_err).into()),
            super::FailureMode::SkipAndReport => {
                report.skipped.push(row_err);
                Ok(())
            }
        }
    }

    /// Commit parsed node units through the shared node-import spine.
    fn commit_nodes(
        &mut self,
        units: Vec<NodeUnit>,
        path: &Path,
        report: &mut Neo4jFidelityReport,
    ) -> Result<()> {
        if units.is_empty() {
            return Ok(());
        }
        let count = units.len();
        let mut iter = units.into_iter();
        let prev = self.config.provenance.take();
        self.config.provenance = Some(provenance_for(path)?);

        let rows: parse::RowIter = Box::new((0..count).map(|_| Ok(Row::new(1, HashMap::new()))));
        let result = self.import_nodes_with(rows, |_row| {
            let unit = iter.next().expect("row/unit counts match");
            fold_note(report, &unit.note);
            Ok(unit.node)
        });
        self.config.provenance = prev;
        let import_report = result?;
        report.nodes_imported += import_report.nodes_imported;
        Ok(())
    }

    /// Commit parsed relationship units through the shared edge-import spine.
    fn commit_edges(
        &mut self,
        units: Vec<EdgeUnit>,
        path: &Path,
        file_label: &str,
        report: &mut Neo4jFidelityReport,
    ) -> Result<()> {
        if units.is_empty() {
            return Ok(());
        }
        let count = units.len();
        let mut iter = units.into_iter();
        let prev = self.config.provenance.take();
        self.config.provenance = Some(provenance_for(path)?);
        let file = file_label.to_string();

        let rows: parse::RowIter = Box::new((0..count).map(|_| Ok(Row::new(1, HashMap::new()))));
        let result = self.import_edges_with(rows, |key_to_id, _row| {
            let unit = iter.next().expect("row/unit counts match");
            let source = key_to_id.get(&unit.source_key).copied().ok_or_else(|| {
                EdgePrepError::Unresolved(UnresolvedEndpoint {
                    row: unit.line,
                    key: unit.source_key.clone(),
                    side: Endpoint::Source,
                    file: Some(file.clone()),
                })
            })?;
            let target = key_to_id.get(&unit.target_key).copied().ok_or_else(|| {
                EdgePrepError::Unresolved(UnresolvedEndpoint {
                    row: unit.line,
                    key: unit.target_key.clone(),
                    side: Endpoint::Target,
                    file: Some(file.clone()),
                })
            })?;
            fold_note(report, &unit.note);
            Ok(PreparedEdge {
                source,
                target,
                label: unit.label,
                properties: unit.properties,
                valid_from: unit.valid_from,
            })
        });
        self.config.provenance = prev;
        let import_report = result?;
        report.relationships_imported += import_report.edges_imported;
        report
            .unresolved_endpoints
            .extend(import_report.unresolved_endpoints);
        Ok(())
    }
}

/// Parse a single node `CREATE` statement into a [`NodeUnit`].
fn build_node_stmt(
    text: &str,
    opts: &Neo4jCypherOptions,
    seq: usize,
) -> std::result::Result<NodeUnit, String> {
    let mut cur = Cursor::new(text);
    let mut note = FidelityNote::default();
    cur.consume_keyword("CREATE")?;
    let pattern = parse_node_pattern(&mut cur, opts, &mut note)?;
    // Merge any coercions recorded during pattern parsing (values) into the node.
    let mut unit = build_node(pattern, opts, seq)?;
    // Fold value-level coercions/unsupported captured on the pattern note.
    for (c, k) in note.coercions {
        unit.note.note_coercion(&c, k);
    }
    for (c, t) in note.unsupported {
        unit.note.note_unsupported(&c, &t);
    }
    Ok(unit)
}
