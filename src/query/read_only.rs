//! Read-only statement guard shared by the MCP `query` tool and the HTTP
//! `/query` endpoint's RBAC classification (Issues #3234, #3350).
//!
//! Detects mutating clauses in a Cypher/AQL statement *before* execution so
//! callers can reject writes (MCP read-only `query` tool) or classify the
//! statement as requiring write access (HTTP authorization).

/// Clauses that would mutate state. The read-only `query` tool rejects any
/// statement containing one of these (as a whole token) before execution.
const MUTATING_KEYWORDS: &[&str] = &[
    "CREATE", "MERGE", "SET", "DELETE", "REMOVE", "DETACH", "DROP", "CALL", "FOREACH", "LOAD",
];

/// Scan a query string for a mutating clause, ignoring string-literal contents
/// (so `{name: 'DELETE'}` does not trip the guard), single-line `//` comments,
/// node labels (`:CALL`), and property keys (`n.set`). Returns the offending
/// keyword so the error can name it.
#[must_use]
pub fn detect_mutating_clause(query: &str) -> Option<&'static str> {
    // First pass: strip string literals (single and double quoted, with backslash
    // escapes) and single-line comments (//) into a sanitized string.
    let mut sanitized = String::with_capacity(query.len());
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(q) => {
                if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
                // Inside a string literal — don't emit characters.
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == '/' && chars.peek() == Some(&'/') {
                    // Single-line comment: skip to end of line.
                    for next in chars.by_ref() {
                        if next == '\n' {
                            break;
                        }
                    }
                } else {
                    sanitized.push(c);
                }
            }
        }
    }

    // Second pass: tokenise and match, but skip tokens immediately preceded by
    // ':' or '.' so that node labels (`:CALL`) and property keys (`n.set`) do
    // not trigger a false positive.
    let mut last_non_ws: Option<char> = None;
    let mut current_token = String::new();

    for c in sanitized.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' {
            current_token.push(c);
        } else {
            if !current_token.is_empty() {
                let preceded_by_label_or_prop =
                    last_non_ws == Some(':') || last_non_ws == Some('.');
                if !preceded_by_label_or_prop
                    && let Some(kw) = MUTATING_KEYWORDS
                        .iter()
                        .copied()
                        .find(|kw| kw.eq_ignore_ascii_case(&current_token))
                {
                    return Some(kw);
                }
                current_token.clear();
            }
            if !c.is_whitespace() {
                last_non_ws = Some(c);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_statements_pass() {
        assert_eq!(detect_mutating_clause("MATCH (n:Person) RETURN n"), None);
        assert_eq!(
            detect_mutating_clause("MATCH (n {name: 'DELETE'}) RETURN n"),
            None
        );
        assert_eq!(detect_mutating_clause("MATCH (n:CALL) RETURN n.set"), None);
    }

    #[test]
    fn mutating_statements_are_detected() {
        assert_eq!(
            detect_mutating_clause("CREATE (n:Person {name: 'X'})"),
            Some("CREATE")
        );
        assert_eq!(
            detect_mutating_clause("MATCH (n) DETACH DELETE n"),
            Some("DETACH")
        );
        assert_eq!(detect_mutating_clause("match (n) set n.x = 1"), Some("SET"));
    }
}
