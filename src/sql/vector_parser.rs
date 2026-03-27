//! Vector operation preprocessing for SQL queries.
//!
//! Extracts vector-related syntax from SQL before passing to sqlparser-rs,
//! similar to how `temporal_parser.rs` handles temporal clauses and
//! `match_parser.rs` handles graph patterns.
//!
//! # Supported Syntax
//!
//! ## Distance operators in ORDER BY
//!
//! ```sql
//! ORDER BY embedding <=> '[0.1, 0.2, 0.3]'::vector LIMIT 10
//! ORDER BY embedding <=> $query_embedding LIMIT 10
//! ORDER BY embedding <-> '[0.1, 0.2, 0.3]'::vector LIMIT 10
//! ```
//!
//! - `<=>` = cosine distance
//! - `<->` = L2 (Euclidean) distance
//!
//! ## KNN function in FROM clause
//!
//! ```sql
//! SELECT * FROM KNN('Documents', 'embedding', $query, 10)
//! ```
//!
//! ## SIMILAR_TO in WHERE clause
//!
//! ```sql
//! WHERE SIMILAR_TO(embedding, $query_embedding, 0.8)
//! ```

use super::error::SqlError;
use crate::index::vector::DistanceMetric;

/// Result of extracting vector clauses from SQL.
#[derive(Debug)]
pub struct ExtractedVector {
    /// The SQL string with vector clauses removed/replaced.
    pub cleaned_sql: String,
    /// Parsed vector operations.
    pub vector_ops: Vec<VectorOp>,
}

/// A parsed vector operation extracted from SQL.
#[derive(Debug)]
pub enum VectorOp {
    /// `ORDER BY property <=> vector LIMIT k`
    KnnOrderBy {
        property_key: String,
        embedding_ref: EmbeddingRef,
        metric: DistanceMetric,
    },
    /// `FROM KNN('label', 'property', embedding, k)`
    KnnFunction {
        /// The node label extracted from KNN's first argument.
        /// Used during SQL rewriting; kept here for debugging/display.
        #[allow(dead_code)]
        label: String,
        property_key: String,
        embedding_ref: EmbeddingRef,
        k: usize,
    },
    /// `WHERE SIMILAR_TO(property, embedding, threshold)`
    SimilarToFilter {
        property_key: String,
        embedding_ref: EmbeddingRef,
        threshold: f64,
    },
}

/// Reference to an embedding value — either inline literal or parameter.
#[derive(Debug, Clone)]
pub enum EmbeddingRef {
    /// A parameter reference like `$query_embedding`.
    Parameter(String),
    /// An inline vector literal like `[0.1, 0.2]`.
    Literal(Vec<f32>),
}

/// Extract vector clauses from a SQL string.
///
/// Detects and removes:
/// 1. `ORDER BY prop <=> vector_ref` distance operators
/// 2. `FROM KNN(...)` function calls
/// 3. `WHERE SIMILAR_TO(...)` function calls
///
/// Returns cleaned SQL suitable for sqlparser-rs plus parsed vector ops.
pub fn extract_vector_clauses(sql: &str) -> Result<ExtractedVector, SqlError> {
    let mut cleaned = sql.to_string();
    let mut vector_ops = Vec::new();

    // 1. Extract KNN function from FROM clause
    extract_knn_function(&mut cleaned, &mut vector_ops)?;

    // 2. Extract SIMILAR_TO from WHERE clause
    extract_similar_to(&mut cleaned, &mut vector_ops)?;

    // 3. Extract ORDER BY distance operators
    extract_order_by_distance(&mut cleaned, &mut vector_ops)?;

    Ok(ExtractedVector {
        cleaned_sql: cleaned,
        vector_ops,
    })
}

/// Extract `FROM KNN('label', 'property', $embedding, k)` from SQL.
///
/// Replaces the KNN function call with the label as a normal table reference.
fn extract_knn_function(sql: &mut String, ops: &mut Vec<VectorOp>) -> Result<(), SqlError> {
    // Case-insensitive search for KNN( outside string literals
    let upper = sql.to_uppercase();
    let Some(knn_pos) = find_outside_strings(&upper, "KNN(") else {
        return Ok(());
    };

    // Find the matching closing paren
    let open_paren = knn_pos + 3; // position of '('
    let close_paren = find_matching_paren(sql, open_paren).ok_or_else(|| {
        SqlError::ParseError("Unmatched parenthesis in KNN() function".to_string())
    })?;

    // Extract the arguments between the parens
    let args_str = &sql[open_paren + 1..close_paren];
    let args = split_args(args_str)?;

    if args.len() != 4 {
        return Err(SqlError::ParseError(format!(
            "KNN() requires 4 arguments (label, property, embedding, k), got {}",
            args.len()
        )));
    }

    let label = unquote_string(&args[0])?;
    let property_key = unquote_string(&args[1])?;
    let embedding_ref = parse_embedding_ref(&args[2])?;
    let k = args[3].trim().parse::<usize>().map_err(|_| {
        SqlError::ParseError(format!("Invalid k value in KNN(): '{}'", args[3].trim()))
    })?;

    ops.push(VectorOp::KnnFunction {
        label: label.clone(),
        property_key,
        embedding_ref,
        k,
    });

    // Replace `KNN(...)` with the label name as a plain table reference
    let replacement = label.to_lowercase();
    sql.replace_range(knn_pos..close_paren + 1, &replacement);

    Ok(())
}

/// Extract `SIMILAR_TO(property, $embedding, threshold)` from WHERE clause.
///
/// Replaces the SIMILAR_TO call with `1=1` so sqlparser can still parse the WHERE.
fn extract_similar_to(sql: &mut String, ops: &mut Vec<VectorOp>) -> Result<(), SqlError> {
    let upper = sql.to_uppercase();
    let Some(st_pos) = find_outside_strings(&upper, "SIMILAR_TO(") else {
        return Ok(());
    };

    let open_paren = st_pos + 10; // position of '('
    let close_paren = find_matching_paren(sql, open_paren).ok_or_else(|| {
        SqlError::ParseError("Unmatched parenthesis in SIMILAR_TO() function".to_string())
    })?;

    let args_str = &sql[open_paren + 1..close_paren];
    let args = split_args(args_str)?;

    if args.len() != 3 {
        return Err(SqlError::ParseError(format!(
            "SIMILAR_TO() requires 3 arguments (property, embedding, threshold), got {}",
            args.len()
        )));
    }

    let property_key = args[0].trim().to_string();
    let embedding_ref = parse_embedding_ref(&args[1])?;
    let threshold = args[2].trim().parse::<f64>().map_err(|_| {
        SqlError::ParseError(format!(
            "Invalid threshold in SIMILAR_TO(): '{}'",
            args[2].trim()
        ))
    })?;

    ops.push(VectorOp::SimilarToFilter {
        property_key,
        embedding_ref,
        threshold,
    });

    // Replace SIMILAR_TO(...) with TRUE so sqlparser can handle the WHERE clause
    sql.replace_range(st_pos..close_paren + 1, "TRUE");

    // Clean up "WHERE TRUE" if it's the only predicate remaining
    cleanup_where_true(sql);

    Ok(())
}

/// Extract `ORDER BY property <=> vector_ref` from SQL.
///
/// Removes the distance-based ORDER BY clause from the SQL, leaving only
/// LIMIT/OFFSET for sqlparser to handle.
fn extract_order_by_distance(sql: &mut String, ops: &mut Vec<VectorOp>) -> Result<(), SqlError> {
    // Look for `<=>` or `<->` outside string literals
    let upper = sql.to_uppercase();

    let (op_pos, op_len, metric) = if let Some(pos) = find_outside_strings(sql, "<=>") {
        (pos, 3, DistanceMetric::Cosine)
    } else if let Some(pos) = find_outside_strings(sql, "<->") {
        (pos, 3, DistanceMetric::Euclidean)
    } else {
        return Ok(());
    };

    // Find the ORDER BY that governs this distance operator.
    // Search backwards from the operator position for "ORDER BY".
    let before = &upper[..op_pos];
    let order_by_pos = before.rfind("ORDER BY").ok_or_else(|| {
        SqlError::ParseError(
            "Distance operator <=> / <-> must appear in an ORDER BY clause".to_string(),
        )
    })?;

    // Extract property name: between "ORDER BY" and the operator.
    let between = sql[order_by_pos + 8..op_pos].trim();
    let property_key = extract_property_from_order_by(between)?;

    // Extract vector reference: from after the operator to the next keyword or end.
    let after_op = op_pos + op_len;
    let (embedding_ref, ref_end) = extract_vector_ref_from_sql(sql, after_op)?;

    ops.push(VectorOp::KnnOrderBy {
        property_key,
        embedding_ref,
        metric,
    });

    // Remove the ORDER BY distance clause.
    // Keep everything before ORDER BY and everything after the vector ref.
    let remainder = sql[ref_end..].trim_start().to_string();
    let prefix = sql[..order_by_pos].to_string();

    *sql = format!("{}{}", prefix, remainder);

    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

/// If the WHERE clause is now just `WHERE TRUE` (or `WHERE TRUE AND ...` was reduced),
/// remove the standalone `WHERE TRUE` to avoid confusing the SQL converter.
fn cleanup_where_true(sql: &mut String) {
    let upper = sql.to_uppercase();
    let Some(where_pos) = find_outside_strings(&upper, "WHERE") else {
        return;
    };

    // Find the start of the predicate content after WHERE
    let predicate_start = where_pos + 5; // len("WHERE")
    let whitespace_len = sql[predicate_start..]
        .len()
        .saturating_sub(sql[predicate_start..].trim_start().len());
    let true_pos = predicate_start + whitespace_len;

    let remainder_upper = sql[true_pos..].to_uppercase();
    let Some(after_true) = remainder_upper.strip_prefix("TRUE") else {
        return;
    };

    let true_end = true_pos + 4; // len("TRUE")
    let after_true_trimmed = after_true.trim_start();

    if after_true_trimmed.is_empty()
        || after_true_trimmed.starts_with("ORDER")
        || after_true_trimmed.starts_with("LIMIT")
        || after_true_trimmed.starts_with("OFFSET")
        || after_true_trimmed.starts_with(';')
    {
        // Standalone WHERE TRUE — remove "WHERE TRUE" entirely
        sql.replace_range(where_pos..true_end, "");
        let trimmed = sql.replace("  ", " ");
        *sql = trimmed.trim().to_string();
    } else if after_true_trimmed.starts_with("AND") {
        // WHERE TRUE AND other → WHERE other
        // Remove from "TRUE" through "AND " (including trailing space)
        let and_pos = true_end + (after_true.len() - after_true_trimmed.len());
        let and_end = and_pos + 3; // len("AND")
        let after_and_ws = sql[and_end..].len() - sql[and_end..].trim_start().len();
        sql.replace_range(true_pos..and_end + after_and_ws, "");
    }
}

/// Find a substring outside of single-quoted string literals.
/// Returns the byte position of the first match, or None.
fn find_outside_strings(sql: &str, needle: &str) -> Option<usize> {
    let chars: Vec<char> = sql.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let needle_len = needle_chars.len();

    let mut i = 0;
    while i < chars.len() {
        // Skip single-quoted strings
        if chars[i] == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        i += 2; // escaped quote
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

        // Check for needle match
        if i + needle_len <= chars.len() {
            let candidate: String = chars[i..i + needle_len].iter().collect();
            if candidate == needle {
                // Convert char index to byte offset
                let byte_offset: usize = chars[..i].iter().map(|c| c.len_utf8()).sum();
                return Some(byte_offset);
            }
            // Also check case-insensitive for keyword-like needles (not operators)
            if !needle.starts_with('<')
                && !needle.starts_with('>')
                && candidate.to_uppercase() == needle.to_uppercase()
            {
                let byte_offset: usize = chars[..i].iter().map(|c| c.len_utf8()).sum();
                return Some(byte_offset);
            }
        }

        i += 1;
    }

    None
}

/// Find the matching closing parenthesis for an opening paren at `open_pos`.
fn find_matching_paren(sql: &str, open_pos: usize) -> Option<usize> {
    let bytes = sql.as_bytes();
    if bytes.get(open_pos) != Some(&b'(') {
        return None;
    }

    let mut depth = 1;
    let mut i = open_pos + 1;
    let mut in_string = false;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
        } else {
            match bytes[i] {
                b'\'' => in_string = true,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }

    None
}

/// Split function arguments by comma, respecting parentheses and string literals.
fn split_args(args: &str) -> Result<Vec<String>, SqlError> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_string = false;

    for ch in args.chars() {
        if in_string {
            current.push(ch);
            if ch == '\'' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '\'' => {
                in_string = true;
                current.push(ch);
            }
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                result.push(current.trim().to_string());
                current = String::new();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }

    Ok(result)
}

/// Remove surrounding single or double quotes from a string literal.
fn unquote_string(s: &str) -> Result<String, SqlError> {
    let s = s.trim();
    if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
        Ok(s[1..s.len() - 1].to_string())
    } else {
        Err(SqlError::ParseError(format!(
            "Expected quoted string, got: '{}'",
            s
        )))
    }
}

/// Parse an embedding reference — either `$param_name` or `'[0.1, 0.2]'::vector`.
fn parse_embedding_ref(s: &str) -> Result<EmbeddingRef, SqlError> {
    let s = s.trim();

    // Parameter reference: $name
    if s.starts_with('$') {
        return Ok(EmbeddingRef::Parameter(s.to_string()));
    }

    // Vector literal: '[0.1, 0.2, 0.3]'::vector or '[0.1, 0.2, 0.3]'
    if s.starts_with('\'') {
        let values = parse_vector_literal(s)?;
        return Ok(EmbeddingRef::Literal(values));
    }

    // Bare identifier (could be a parameter without $)
    if s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Ok(EmbeddingRef::Parameter(format!("${}", s)));
    }

    Err(SqlError::ParseError(format!(
        "Cannot parse embedding reference: '{}'",
        s
    )))
}

/// Parse a vector literal like `'[0.1, 0.2, 0.3]'::vector` or `'[0.1, 0.2, 0.3]'`.
fn parse_vector_literal(s: &str) -> Result<Vec<f32>, SqlError> {
    let s = s.trim();

    // Strip ::vector suffix if present
    let s = if let Some(cast_pos) = s.find("::") {
        &s[..cast_pos]
    } else {
        s
    };

    // Strip surrounding single quotes
    let s = if s.starts_with('\'') && s.ends_with('\'') {
        &s[1..s.len() - 1]
    } else {
        s
    };

    // Strip brackets
    let s = s.trim();
    let s = s
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| {
            SqlError::ParseError(format!("Vector literal must be enclosed in []: '{}'", s))
        })?;

    // Parse comma-separated floats
    let values: Result<Vec<f32>, _> = s.split(',').map(|v| v.trim().parse::<f32>()).collect();
    values.map_err(|e| SqlError::ParseError(format!("Invalid vector literal: {}", e)))
}

/// Extract property name from ORDER BY text before the distance operator.
///
/// Handles plain identifiers and `table.column` forms.
fn extract_property_from_order_by(text: &str) -> Result<String, SqlError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(SqlError::ParseError(
            "Missing property name in ORDER BY distance clause".to_string(),
        ));
    }

    // Handle `table.column` form — take the column part
    if let Some(dot_pos) = text.rfind('.') {
        let col = text[dot_pos + 1..].trim();
        if !col.is_empty() {
            return Ok(col.to_string());
        }
    }

    // Plain identifier
    Ok(text.to_string())
}

/// Extract a vector reference from SQL starting at `start`, returning the
/// parsed reference and the byte position just past it.
fn extract_vector_ref_from_sql(sql: &str, start: usize) -> Result<(EmbeddingRef, usize), SqlError> {
    let rest = sql[start..].trim_start();
    let trimmed_offset = start + (sql.len() - start - rest.len());

    // Parameter reference: $name
    if rest.starts_with('$') {
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
            .unwrap_or(rest.len());
        let param = &rest[..end];
        return Ok((
            EmbeddingRef::Parameter(param.to_string()),
            trimmed_offset + end,
        ));
    }

    // Vector literal: '[...]'::vector
    if let Some(after_quote) = rest.strip_prefix('\'') {
        // Find the closing quote (position relative to rest, not after_quote)
        let close_quote = after_quote.find('\'').map(|p| p + 1).ok_or_else(|| {
            SqlError::ParseError("Unterminated vector literal in ORDER BY".to_string())
        })?;

        let mut end = close_quote + 1;

        // Check for ::vector cast suffix
        if rest[end..].starts_with("::") {
            // Find end of cast type
            let cast_end = rest[end + 2..]
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .map(|p| end + 2 + p)
                .unwrap_or(rest.len());
            end = cast_end;
        }

        let literal_text = &rest[..end];
        let values = parse_vector_literal(literal_text)?;
        return Ok((EmbeddingRef::Literal(values), trimmed_offset + end));
    }

    Err(SqlError::ParseError(
        "Expected vector literal ('[...]'::vector) or parameter ($name) after distance operator"
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Vector Literal Parsing
    // ========================================================================

    #[test]
    fn test_parse_vector_literal_basic() {
        let values = parse_vector_literal("'[0.1, 0.2, 0.3]'::vector").unwrap();
        assert_eq!(values, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn test_parse_vector_literal_without_cast() {
        let values = parse_vector_literal("'[1.0, 2.0]'").unwrap();
        assert_eq!(values, vec![1.0, 2.0]);
    }

    #[test]
    fn test_parse_vector_literal_invalid() {
        assert!(parse_vector_literal("'not a vector'").is_err());
    }

    // ========================================================================
    // Embedding Reference Parsing
    // ========================================================================

    #[test]
    fn test_parse_embedding_ref_parameter() {
        let r = parse_embedding_ref("$query_embedding").unwrap();
        assert!(matches!(r, EmbeddingRef::Parameter(ref s) if s == "$query_embedding"));
    }

    #[test]
    fn test_parse_embedding_ref_literal() {
        let r = parse_embedding_ref("'[0.1, 0.2]'::vector").unwrap();
        assert!(matches!(r, EmbeddingRef::Literal(ref v) if v.len() == 2));
    }

    // ========================================================================
    // ORDER BY Distance Operator Extraction
    // ========================================================================

    #[test]
    fn test_extract_order_by_cosine_literal() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents ORDER BY embedding <=> '[0.1, 0.2, 0.3]'::vector LIMIT 10",
        )
        .unwrap();

        assert_eq!(result.vector_ops.len(), 1);
        assert!(matches!(
            &result.vector_ops[0],
            VectorOp::KnnOrderBy {
                property_key,
                metric: DistanceMetric::Cosine,
                ..
            } if property_key == "embedding"
        ));

        // Cleaned SQL should not contain <=>
        assert!(!result.cleaned_sql.contains("<=>"));
        // Should still have LIMIT
        assert!(result.cleaned_sql.to_uppercase().contains("LIMIT"));
    }

    #[test]
    fn test_extract_order_by_cosine_parameter() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents ORDER BY embedding <=> $query_embedding LIMIT 10",
        )
        .unwrap();

        assert_eq!(result.vector_ops.len(), 1);
        assert!(matches!(
            &result.vector_ops[0],
            VectorOp::KnnOrderBy {
                embedding_ref: EmbeddingRef::Parameter(p),
                ..
            } if p == "$query_embedding"
        ));
    }

    #[test]
    fn test_extract_order_by_l2_distance() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents ORDER BY embedding <-> '[0.1, 0.2]'::vector LIMIT 5",
        )
        .unwrap();

        assert!(matches!(
            &result.vector_ops[0],
            VectorOp::KnnOrderBy {
                metric: DistanceMetric::Euclidean,
                ..
            }
        ));
    }

    #[test]
    fn test_extract_order_by_compound_property() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents ORDER BY doc.embedding <=> $query LIMIT 10",
        )
        .unwrap();

        assert!(matches!(
            &result.vector_ops[0],
            VectorOp::KnnOrderBy {
                property_key,
                ..
            } if property_key == "embedding"
        ));
    }

    #[test]
    fn test_extract_order_by_no_limit() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents ORDER BY embedding <=> '[0.1, 0.2]'::vector",
        )
        .unwrap();

        assert_eq!(result.vector_ops.len(), 1);
    }

    // ========================================================================
    // KNN Function Extraction
    // ========================================================================

    #[test]
    fn test_extract_knn_function() {
        let result =
            extract_vector_clauses("SELECT * FROM KNN('Documents', 'embedding', $query, 10)")
                .unwrap();

        assert_eq!(result.vector_ops.len(), 1);
        assert!(matches!(
            &result.vector_ops[0],
            VectorOp::KnnFunction {
                label,
                property_key,
                k: 10,
                ..
            } if label == "Documents" && property_key == "embedding"
        ));

        // KNN(...) should be replaced with the label name
        assert!(result.cleaned_sql.contains("documents"));
        assert!(!result.cleaned_sql.to_uppercase().contains("KNN("));
    }

    #[test]
    fn test_extract_knn_function_wrong_arg_count() {
        let result = extract_vector_clauses("SELECT * FROM KNN('Documents', 'embedding', $query)");
        assert!(result.is_err());
    }

    // ========================================================================
    // SIMILAR_TO Extraction
    // ========================================================================

    #[test]
    fn test_extract_similar_to() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents WHERE SIMILAR_TO(embedding, $query_embedding, 0.8)",
        )
        .unwrap();

        assert_eq!(result.vector_ops.len(), 1);
        assert!(matches!(
            &result.vector_ops[0],
            VectorOp::SimilarToFilter {
                property_key,
                threshold,
                ..
            } if property_key == "embedding" && (*threshold - 0.8).abs() < f64::EPSILON
        ));

        // SIMILAR_TO should be removed from the SQL
        assert!(!result.cleaned_sql.to_uppercase().contains("SIMILAR_TO"));
        // WHERE TRUE is cleaned up, so no WHERE should remain
        assert!(
            !result.cleaned_sql.to_uppercase().contains("WHERE"),
            "WHERE clause should be removed when SIMILAR_TO was the only predicate, got: '{}'",
            result.cleaned_sql
        );
    }

    #[test]
    fn test_extract_similar_to_wrong_arg_count() {
        let result =
            extract_vector_clauses("SELECT * FROM Documents WHERE SIMILAR_TO(embedding, $query)");
        assert!(result.is_err());
    }

    // ========================================================================
    // No Vector Clauses
    // ========================================================================

    #[test]
    fn test_extract_no_vector_clauses() {
        let sql = "SELECT * FROM nodes WHERE label = 'Person' ORDER BY name LIMIT 10";
        let result = extract_vector_clauses(sql).unwrap();

        assert!(result.vector_ops.is_empty());
        assert_eq!(result.cleaned_sql, sql);
    }

    // ========================================================================
    // Combined Operations
    // ========================================================================

    #[test]
    fn test_extract_similar_to_with_and_clause() {
        let result = extract_vector_clauses(
            "SELECT * FROM Documents WHERE SIMILAR_TO(embedding, $query, 0.8) AND price < 100",
        )
        .unwrap();

        assert_eq!(result.vector_ops.len(), 1);
        // Cleaned SQL should still have the AND price < 100 clause
        assert!(result.cleaned_sql.contains("price < 100"));
    }
}
