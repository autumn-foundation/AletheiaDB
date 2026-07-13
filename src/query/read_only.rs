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

    /// Issue #560 guardrail: now that `execute_cypher` supports these write
    /// clauses, the text-level read-only guard shared by the MCP `query` tool
    /// and the HTTP `/query` RBAC classification MUST still reject every one of
    /// them *before the parser runs*, so a mutation can never slip through the
    /// read-only surface. This is intentionally redundant with the MCP-layer
    /// tests (`src/mcp/tests.rs`) -- it pins the contract from the guard's own
    /// module, which the Cypher lane owns.
    #[test]
    fn issue_560_write_clauses_stay_rejected_by_the_read_only_guard() {
        let cases = [
            ("CREATE (n:Person {name: 'Zed'}) RETURN n", "CREATE"),
            ("MATCH (n:Person) SET n.age = 41 RETURN n", "SET"),
            ("MATCH (n:Person {name: 'Alice'}) DELETE n", "DELETE"),
            ("MATCH (n:Person) DETACH DELETE n", "DETACH"),
            ("MATCH (n:Person) REMOVE n.age", "REMOVE"),
            ("MERGE (n:Person {name: 'Zed'}) RETURN n", "MERGE"),
            (
                "MATCH (a:Person {name: 'A'})-[r:KNOWS]->(b) DELETE r",
                "DELETE",
            ),
            ("CREATE (a:Team)-[:HAS]->(b:Member) RETURN a, b", "CREATE"),
        ];
        for (query, expected) in cases {
            assert_eq!(
                detect_mutating_clause(query),
                Some(expected),
                "read-only guard must reject `{query}` (found no mutating clause)"
            );
        }
    }

    /// The read-only guard must not be fooled into a *false positive* by a
    /// mutating keyword that only appears as string-literal content, a node
    /// label, or a property key -- a plain read stays allowed even next to those.
    #[test]
    fn issue_560_reads_with_keywordish_identifiers_still_pass() {
        assert_eq!(
            detect_mutating_clause("MATCH (n:Person {note: 'please CREATE later'}) RETURN n"),
            None
        );
        assert_eq!(
            detect_mutating_clause("MATCH (n:Create) RETURN n.delete"),
            None
        );
    }

    /// Issue #560 false-positive hardening: a mutating keyword that appears only
    /// inside a DOUBLE-quoted string literal, or as a substring of an ordinary
    /// identifier / property key, must NOT trip the guard — a read stays allowed.
    /// But a mutating keyword in any letter-case IS a whole-token match and must
    /// be rejected.
    #[test]
    fn issue_560_guard_false_positive_and_case_hardening() {
        // Double-quoted literal containing a keyword → read is allowed.
        assert_eq!(
            detect_mutating_clause(r#"MATCH (n {note: "DELETE"}) RETURN n"#),
            None
        );
        // Substring identifiers (`created` / `deleted`) as a property key or
        // variable are not the whole tokens CREATE / DELETE → allowed.
        assert_eq!(
            detect_mutating_clause("MATCH (n:Event) RETURN n.created, n.deleted"),
            None
        );
        assert_eq!(
            detect_mutating_clause("MATCH (created) RETURN created"),
            None
        );
        // A mixed-case mutating keyword is still a whole-token match → rejected
        // (the guard is case-insensitive, matching the case-insensitive lexer).
        assert_eq!(detect_mutating_clause("CrEaTe (n)"), Some("CREATE"));
        assert_eq!(detect_mutating_clause("match (n) DeLeTe n"), Some("DELETE"));
    }
}
