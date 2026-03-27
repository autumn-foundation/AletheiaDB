//! MATCH clause preprocessing for graph pattern parsing.
//!
//! Extracts MATCH clauses from SQL before passing to sqlparser-rs,
//! similar to how temporal_parser.rs handles temporal clauses.
//!
//! # Approach
//!
//! Since sqlparser-rs doesn't support graph pattern matching syntax, we:
//! 1. Find `MATCH` keyword in SQL (case-insensitive, outside string literals)
//! 2. Extract the graph pattern between MATCH and the next SQL keyword
//! 3. Parse patterns like `(source)-[:KNOWS*1..3]->(target)`
//! 4. Remove the MATCH clause from the SQL string
//! 5. Convert patterns to `QueryOp` traversal operations
//!
//! # Supported Patterns
//!
//! - `(a)-[:KNOWS]->(b)` - Outgoing traversal, single hop
//! - `(a)<-[:PARENT_OF]-(b)` - Incoming traversal, single hop
//! - `(a)-[:RELATED]-(b)` - Bidirectional traversal, single hop
//! - `(a)-[:KNOWS*2]->(b)` - Exact depth
//! - `(a)-[:KNOWS*1..3]->(b)` - Range depth
//! - `(a)-[:KNOWS*]->(b)` - Variable (unbounded) depth
//! - `(a)-[:KNOWS*2..]->(b)` - Open-ended min (default max=10)
//! - `(a)-[:KNOWS*..3]->(b)` - Open-ended max (default min=1)

use super::error::SqlError;
use crate::query::ir::{QueryOp, TraversalDepth};

/// Default maximum depth for open-ended range patterns like `*2..`.
const DEFAULT_MAX_DEPTH: usize = 10;

/// A parsed graph pattern from a MATCH clause.
#[derive(Debug, Clone)]
pub struct GraphPattern {
    /// Alias of the source node in the pattern (used for binding in future multi-pattern support).
    #[allow(dead_code)]
    pub source_alias: String,
    /// Alias of the target node in the pattern (used for binding in future multi-pattern support).
    #[allow(dead_code)]
    pub target_alias: String,
    /// Edge type label (e.g., "KNOWS"), or None for any edge.
    pub edge_type: Option<String>,
    /// Direction of the traversal.
    pub direction: PatternDirection,
    /// Depth specification for the traversal.
    pub depth: TraversalDepth,
}

/// Direction of a graph pattern edge.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternDirection {
    /// `(a)-[:TYPE]->(b)` - Follow outgoing edges.
    Outgoing,
    /// `(a)<-[:TYPE]-(b)` - Follow incoming edges.
    Incoming,
    /// `(a)-[:TYPE]-(b)` - Follow edges in both directions.
    Both,
}

/// Result of extracting MATCH clauses from SQL.
#[derive(Debug)]
pub struct ExtractedMatch {
    /// The SQL string with MATCH clauses removed.
    pub cleaned_sql: String,
    /// Parsed graph patterns from the MATCH clause.
    pub patterns: Vec<GraphPattern>,
}

impl GraphPattern {
    /// Convert this graph pattern to the corresponding `QueryOp`.
    pub fn to_query_op(&self) -> QueryOp {
        match self.direction {
            PatternDirection::Outgoing => QueryOp::TraverseOut {
                label: self.edge_type.clone(),
                depth: self.depth,
            },
            PatternDirection::Incoming => QueryOp::TraverseIn {
                label: self.edge_type.clone(),
                depth: self.depth,
            },
            PatternDirection::Both => QueryOp::TraverseBoth {
                label: self.edge_type.clone(),
                depth: self.depth,
            },
        }
    }
}

/// Extract MATCH clauses from a SQL string.
///
/// Finds `MATCH` keyword outside of string literals, extracts the graph
/// pattern, parses it, and returns the cleaned SQL plus parsed patterns.
///
/// # Errors
///
/// Returns `SqlError::ParseError` if the MATCH clause contains an invalid
/// graph pattern.
pub fn extract_match_clauses(sql: &str) -> Result<ExtractedMatch, SqlError> {
    let mut cleaned = sql.to_string();
    let mut patterns = Vec::new();

    while let Some(match_pos) = find_keyword_outside_strings(&cleaned, "MATCH") {
        // Find where the MATCH clause ends (next SQL keyword or end of string)
        let after_match = match_pos + "MATCH".len();
        let match_end = find_match_end(&cleaned, after_match);

        // Extract the pattern text between MATCH and the next keyword
        let pattern_text = cleaned[after_match..match_end].trim().to_string();

        if pattern_text.is_empty() {
            return Err(SqlError::ParseError("Empty MATCH pattern".to_string()));
        }

        // Parse the graph pattern(s)
        let parsed = parse_graph_patterns(&pattern_text)?;
        patterns.extend(parsed);

        // Remove the MATCH clause from cleaned SQL
        cleaned = format!(
            "{} {}",
            cleaned[..match_pos].trim_end(),
            cleaned[match_end..].trim_start()
        );
    }

    Ok(ExtractedMatch {
        cleaned_sql: cleaned,
        patterns,
    })
}

/// Find a keyword in SQL that is NOT inside a string literal.
///
/// Returns the byte offset of the first character of the keyword, or None.
fn find_keyword_outside_strings(sql: &str, keyword: &str) -> Option<usize> {
    let sql_upper = sql.to_uppercase();
    let keyword_upper = keyword.to_uppercase();
    let keyword_len = keyword_upper.chars().count();

    let mut i = 0;
    let chars: Vec<char> = sql.chars().collect();
    let upper_chars: Vec<char> = sql_upper.chars().collect();

    while i < chars.len() {
        // Skip string literals
        if chars[i] == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    // Check for escaped quote ''
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // Check if we have the keyword at this position
        if i + keyword_len <= upper_chars.len() {
            let candidate: String = upper_chars[i..i + keyword_len].iter().collect();
            if candidate == keyword_upper {
                // Ensure it's a word boundary (not part of a larger identifier)
                let before_ok = i == 0 || !chars[i - 1].is_alphanumeric() && chars[i - 1] != '_';
                let after_ok = i + keyword_len >= chars.len()
                    || !chars[i + keyword_len].is_alphanumeric() && chars[i + keyword_len] != '_';

                if before_ok && after_ok {
                    // Calculate byte offset
                    let byte_offset: usize = sql.chars().take(i).map(|c| c.len_utf8()).sum();
                    return Some(byte_offset);
                }
            }
        }

        i += 1;
    }

    None
}

/// Find where the MATCH clause ends.
///
/// The MATCH clause ends at the next SQL keyword that follows the pattern.
/// These keywords are: WHERE, ORDER, LIMIT, OFFSET, GROUP, HAVING, or end of string.
fn find_match_end(sql: &str, start: usize) -> usize {
    let remainder = &sql[start..];
    let keywords = [
        "WHERE", "ORDER", "LIMIT", "OFFSET", "GROUP", "HAVING", "MATCH",
    ];

    // Find the earliest keyword outside string literals
    let mut earliest: Option<usize> = None;

    for kw in &keywords {
        if let Some(pos) = find_keyword_outside_strings(remainder, kw) {
            let absolute = start + pos;
            if earliest.is_none_or(|e| absolute < e) {
                earliest = Some(absolute);
            }
        }
    }

    earliest.unwrap_or(sql.len())
}

/// Parse graph patterns from the extracted pattern text.
///
/// Currently supports a single pattern per MATCH clause.
/// Pattern format: `(source)<-[:TYPE*depth]-(target)` with variations.
fn parse_graph_patterns(text: &str) -> Result<Vec<GraphPattern>, SqlError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(SqlError::ParseError("Empty MATCH pattern".to_string()));
    }

    // Parse a single pattern: (alias) <edge_spec> (alias)
    let pattern = parse_single_pattern(text)?;
    Ok(vec![pattern])
}

/// Parse a single graph pattern like `(source)-[:KNOWS*1..3]->(target)`.
fn parse_single_pattern(text: &str) -> Result<GraphPattern, SqlError> {
    // Detect direction and split the pattern into parts.
    //
    // Possible shapes:
    //   Outgoing:      (src)-[:TYPE]->(tgt)
    //   Incoming:      (src)<-[:TYPE]-(tgt)
    //   Bidirectional: (src)-[:TYPE]-(tgt)
    //
    // Strategy: find the edge bracket `[...]` and examine the surrounding
    // characters to determine direction.

    let bracket_start = text.find('[').ok_or_else(|| {
        SqlError::ParseError(format!("Invalid MATCH pattern: missing '[' in '{}'", text))
    })?;

    let bracket_end = text.find(']').ok_or_else(|| {
        SqlError::ParseError(format!("Invalid MATCH pattern: missing ']' in '{}'", text))
    })?;

    if bracket_end <= bracket_start {
        return Err(SqlError::ParseError(format!(
            "Invalid MATCH pattern: ']' before '[' in '{}'",
            text
        )));
    }

    // Extract the source alias: everything before the edge start
    // For outgoing/bidirectional: (source)- ... so before `-[`
    // For incoming: (source)<- ... so before `<-[`
    let before_bracket = &text[..bracket_start];
    let after_bracket = &text[bracket_end + 1..];

    // Determine direction by examining characters around the brackets.
    let direction = determine_direction(before_bracket, after_bracket)?;

    // Extract source alias from the first parenthesized group
    let source_alias = extract_alias_from_start(text)?;

    // Extract target alias from the last parenthesized group
    let target_alias = extract_alias_from_end(text)?;

    // Extract edge type and depth from the bracket content
    let bracket_content = &text[bracket_start + 1..bracket_end];
    let (edge_type, depth) = parse_edge_spec(bracket_content)?;

    Ok(GraphPattern {
        source_alias,
        target_alias,
        edge_type,
        direction,
        depth,
    })
}

/// Determine the pattern direction from the characters around the edge brackets.
fn determine_direction(
    before_bracket: &str,
    after_bracket: &str,
) -> Result<PatternDirection, SqlError> {
    let before = before_bracket.trim_end();
    let after = after_bracket.trim_start();

    // Incoming: `<-[...]-(` - before ends with `<-` and after starts with `-(`
    if before.ends_with("<-") {
        return Ok(PatternDirection::Incoming);
    }

    // Outgoing: `-[...]->(` - after starts with `->`
    if after.starts_with("->") {
        return Ok(PatternDirection::Outgoing);
    }

    // Bidirectional: `-[...]-(` - after starts with `-` but not `->`
    if after.starts_with('-') {
        return Ok(PatternDirection::Both);
    }

    Err(SqlError::ParseError(format!(
        "Cannot determine direction in MATCH pattern: before='{}', after='{}'",
        before_bracket, after_bracket
    )))
}

/// Extract the alias from the first parenthesized group in the pattern.
fn extract_alias_from_start(text: &str) -> Result<String, SqlError> {
    let open = text.find('(').ok_or_else(|| {
        SqlError::ParseError(format!(
            "Invalid MATCH pattern: missing '(' for source alias in '{}'",
            text
        ))
    })?;
    let close = text.find(')').ok_or_else(|| {
        SqlError::ParseError(format!(
            "Invalid MATCH pattern: missing ')' for source alias in '{}'",
            text
        ))
    })?;

    if close <= open {
        return Err(SqlError::ParseError(format!(
            "Invalid MATCH pattern: ')' before '(' in '{}'",
            text
        )));
    }

    let alias = text[open + 1..close].trim().to_string();
    if alias.is_empty() {
        return Err(SqlError::ParseError(
            "Empty source alias in MATCH pattern".to_string(),
        ));
    }

    Ok(alias)
}

/// Extract the alias from the last parenthesized group in the pattern.
fn extract_alias_from_end(text: &str) -> Result<String, SqlError> {
    let open = text.rfind('(').ok_or_else(|| {
        SqlError::ParseError(format!(
            "Invalid MATCH pattern: missing '(' for target alias in '{}'",
            text
        ))
    })?;
    let close = text.rfind(')').ok_or_else(|| {
        SqlError::ParseError(format!(
            "Invalid MATCH pattern: missing ')' for target alias in '{}'",
            text
        ))
    })?;

    if close <= open {
        return Err(SqlError::ParseError(format!(
            "Invalid MATCH pattern: ')' before '(' for target in '{}'",
            text
        )));
    }

    let alias = text[open + 1..close].trim().to_string();
    if alias.is_empty() {
        return Err(SqlError::ParseError(
            "Empty target alias in MATCH pattern".to_string(),
        ));
    }

    Ok(alias)
}

/// Parse the edge specification inside brackets: `:TYPE*depth`.
///
/// Examples:
/// - `:KNOWS` -> (Some("KNOWS"), Exact(1))
/// - `:KNOWS*2` -> (Some("KNOWS"), Exact(2))
/// - `:KNOWS*1..3` -> (Some("KNOWS"), Range{1,3})
/// - `:KNOWS*` -> (Some("KNOWS"), Variable)
/// - `:KNOWS*2..` -> (Some("KNOWS"), Range{2, DEFAULT_MAX_DEPTH})
/// - `:KNOWS*..3` -> (Some("KNOWS"), Range{1, 3})
fn parse_edge_spec(content: &str) -> Result<(Option<String>, TraversalDepth), SqlError> {
    let content = content.trim();

    if content.is_empty() {
        return Ok((None, TraversalDepth::Exact(1)));
    }

    // Must start with ':'
    if !content.starts_with(':') {
        return Err(SqlError::ParseError(format!(
            "Edge type must start with ':' in '[{}]'",
            content
        )));
    }

    let after_colon = &content[1..];

    // Split on '*' to separate edge type from depth spec
    if let Some(star_pos) = after_colon.find('*') {
        let edge_type = after_colon[..star_pos].trim().to_string();
        let depth_spec = &after_colon[star_pos + 1..];

        let edge_type = if edge_type.is_empty() {
            None
        } else {
            Some(edge_type)
        };

        let depth = parse_depth_spec(depth_spec)?;
        Ok((edge_type, depth))
    } else {
        // No '*' - just edge type, default depth
        let edge_type = after_colon.trim().to_string();
        let edge_type = if edge_type.is_empty() {
            None
        } else {
            Some(edge_type)
        };

        Ok((edge_type, TraversalDepth::Exact(1)))
    }
}

/// Parse a depth specification string (the part after `*`).
///
/// - `` (empty) -> Variable
/// - `2` -> Exact(2)
/// - `1..3` -> Range{1,3}
/// - `2..` -> Range{2, DEFAULT_MAX_DEPTH}
/// - `..3` -> Range{1, 3}
fn parse_depth_spec(spec: &str) -> Result<TraversalDepth, SqlError> {
    let spec = spec.trim();

    // Empty after '*' -> unbounded variable depth
    if spec.is_empty() {
        return Ok(TraversalDepth::Variable);
    }

    // Check for range with '..'
    if let Some(dot_pos) = spec.find("..") {
        let min_str = &spec[..dot_pos];
        let max_str = &spec[dot_pos + 2..];

        let min = if min_str.is_empty() {
            1 // default min
        } else {
            min_str.parse::<usize>().map_err(|_| {
                SqlError::ParseError(format!(
                    "Invalid minimum depth '{}' in traversal spec",
                    min_str
                ))
            })?
        };

        let max = if max_str.is_empty() {
            DEFAULT_MAX_DEPTH // default max for open-ended
        } else {
            max_str.parse::<usize>().map_err(|_| {
                SqlError::ParseError(format!(
                    "Invalid maximum depth '{}' in traversal spec",
                    max_str
                ))
            })?
        };

        if min > max {
            return Err(SqlError::ParseError(format!(
                "Invalid depth range: min ({}) > max ({})",
                min, max
            )));
        }

        Ok(TraversalDepth::Range { min, max })
    } else {
        // Exact depth
        let n = spec.parse::<usize>().map_err(|_| {
            SqlError::ParseError(format!("Invalid depth '{}' in traversal spec", spec))
        })?;
        Ok(TraversalDepth::Exact(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_no_match() {
        let result = extract_match_clauses("SELECT * FROM nodes WHERE x = 1").unwrap();
        assert!(result.patterns.is_empty());
        assert_eq!(result.cleaned_sql, "SELECT * FROM nodes WHERE x = 1");
    }

    #[test]
    fn test_extract_simple_outgoing() {
        let result = extract_match_clauses(
            "SELECT * FROM nodes AS a MATCH (a)-[:KNOWS]->(b) WHERE a.name = 'Alice'",
        )
        .unwrap();

        assert_eq!(result.patterns.len(), 1);
        assert_eq!(result.patterns[0].source_alias, "a");
        assert_eq!(result.patterns[0].target_alias, "b");
        assert_eq!(result.patterns[0].edge_type, Some("KNOWS".to_string()));
        assert_eq!(result.patterns[0].direction, PatternDirection::Outgoing);
        assert_eq!(result.patterns[0].depth, TraversalDepth::Exact(1));

        // Cleaned SQL should not contain MATCH
        assert!(!result.cleaned_sql.to_uppercase().contains("MATCH"));
        assert!(result.cleaned_sql.contains("WHERE"));
    }

    #[test]
    fn test_extract_incoming() {
        let result = extract_match_clauses(
            "SELECT * FROM nodes AS child MATCH (parent)<-[:PARENT_OF]-(child)",
        )
        .unwrap();

        assert_eq!(result.patterns.len(), 1);
        assert_eq!(result.patterns[0].direction, PatternDirection::Incoming);
        assert_eq!(result.patterns[0].edge_type, Some("PARENT_OF".to_string()));
    }

    #[test]
    fn test_extract_bidirectional() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS n MATCH (n)-[:RELATED]-(related)")
                .unwrap();

        assert_eq!(result.patterns.len(), 1);
        assert_eq!(result.patterns[0].direction, PatternDirection::Both);
    }

    #[test]
    fn test_extract_with_variable_depth() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS a MATCH (a)-[:KNOWS*1..3]->(b)").unwrap();

        assert_eq!(
            result.patterns[0].depth,
            TraversalDepth::Range { min: 1, max: 3 }
        );
    }

    #[test]
    fn test_extract_exact_depth() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS a MATCH (a)-[:KNOWS*2]->(b)").unwrap();

        assert_eq!(result.patterns[0].depth, TraversalDepth::Exact(2));
    }

    #[test]
    fn test_extract_unbounded() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS a MATCH (a)-[:KNOWS*]->(b)").unwrap();

        assert_eq!(result.patterns[0].depth, TraversalDepth::Variable);
    }

    #[test]
    fn test_extract_open_min() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS a MATCH (a)-[:KNOWS*2..]->(b)").unwrap();

        assert_eq!(
            result.patterns[0].depth,
            TraversalDepth::Range {
                min: 2,
                max: DEFAULT_MAX_DEPTH
            }
        );
    }

    #[test]
    fn test_extract_open_max() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS a MATCH (a)-[:KNOWS*..3]->(b)").unwrap();

        assert_eq!(
            result.patterns[0].depth,
            TraversalDepth::Range { min: 1, max: 3 }
        );
    }

    #[test]
    fn test_match_inside_string_ignored() {
        let result =
            extract_match_clauses("SELECT * FROM nodes WHERE name = 'MATCH (a)-[:X]->(b)'")
                .unwrap();

        assert!(result.patterns.is_empty());
    }

    #[test]
    fn test_case_insensitive_match() {
        let result =
            extract_match_clauses("SELECT * FROM nodes AS a match (a)-[:KNOWS]->(b)").unwrap();

        assert_eq!(result.patterns.len(), 1);
    }

    #[test]
    fn test_to_query_op_outgoing() {
        let pattern = GraphPattern {
            source_alias: "a".to_string(),
            target_alias: "b".to_string(),
            edge_type: Some("KNOWS".to_string()),
            direction: PatternDirection::Outgoing,
            depth: TraversalDepth::Exact(1),
        };

        let op = pattern.to_query_op();
        assert!(matches!(
            op,
            QueryOp::TraverseOut {
                label: Some(ref l),
                depth: TraversalDepth::Exact(1),
            } if l == "KNOWS"
        ));
    }

    #[test]
    fn test_to_query_op_incoming() {
        let pattern = GraphPattern {
            source_alias: "a".to_string(),
            target_alias: "b".to_string(),
            edge_type: Some("PARENT_OF".to_string()),
            direction: PatternDirection::Incoming,
            depth: TraversalDepth::Exact(1),
        };

        let op = pattern.to_query_op();
        assert!(matches!(op, QueryOp::TraverseIn { .. }));
    }

    #[test]
    fn test_to_query_op_both() {
        let pattern = GraphPattern {
            source_alias: "a".to_string(),
            target_alias: "b".to_string(),
            edge_type: Some("RELATED".to_string()),
            direction: PatternDirection::Both,
            depth: TraversalDepth::Exact(1),
        };

        let op = pattern.to_query_op();
        assert!(matches!(op, QueryOp::TraverseBoth { .. }));
    }

    #[test]
    fn test_cleaned_sql_valid_for_sqlparser() {
        let result = extract_match_clauses(
            "SELECT * FROM nodes AS source MATCH (source)-[:KNOWS]->(target) WHERE source.name = 'Alice' ORDER BY target.name LIMIT 10",
        )
        .unwrap();

        // The cleaned SQL must be parseable by sqlparser
        assert!(result.cleaned_sql.contains("SELECT"));
        assert!(result.cleaned_sql.contains("FROM"));
        assert!(result.cleaned_sql.contains("WHERE"));
        assert!(result.cleaned_sql.contains("ORDER BY"));
        assert!(result.cleaned_sql.contains("LIMIT"));
        assert!(!result.cleaned_sql.to_uppercase().contains("MATCH"));
    }

    #[test]
    fn test_extract_multiple_match_clauses() {
        let result = extract_match_clauses(
            "SELECT * FROM nodes AS a MATCH (a)-[:KNOWS]->(b) MATCH (b)-[:WORKS_AT]->(c) WHERE a.name = 'Alice'",
        )
        .unwrap();

        assert_eq!(result.patterns.len(), 2);
        assert_eq!(result.patterns[0].edge_type, Some("KNOWS".to_string()));
        assert_eq!(result.patterns[0].direction, PatternDirection::Outgoing);
        assert_eq!(result.patterns[1].edge_type, Some("WORKS_AT".to_string()));
        assert_eq!(result.patterns[1].direction, PatternDirection::Outgoing);

        // Cleaned SQL should have NO MATCH keywords remaining
        assert!(!result.cleaned_sql.to_uppercase().contains("MATCH"));
        assert!(result.cleaned_sql.contains("WHERE"));
    }

    #[test]
    fn test_extract_empty_edge_spec() {
        let result = extract_match_clauses("SELECT * FROM nodes AS a MATCH (a)-[]->(b)").unwrap();

        assert_eq!(result.patterns.len(), 1);
        assert_eq!(result.patterns[0].edge_type, None);
        assert_eq!(result.patterns[0].direction, PatternDirection::Outgoing);
        assert_eq!(result.patterns[0].depth, TraversalDepth::Exact(1));
    }
}
