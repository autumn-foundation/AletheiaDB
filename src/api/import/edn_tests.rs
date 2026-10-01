//! Unit tests for the minimal EDN reader (Issue #3384).

use super::edn::{Edn, EdnError, MAX_DEPTH, parse_one, parse_top_vector};

fn parse(s: &str) -> Edn {
    parse_one(s).unwrap_or_else(|e| panic!("parse '{s}' failed: {e}"))
}

// ---- Scalars ---------------------------------------------------------------

#[test]
fn reads_nil() {
    assert_eq!(parse("nil"), Edn::Nil);
}

#[test]
fn reads_true_false() {
    assert_eq!(parse("true"), Edn::Bool(true));
    assert_eq!(parse("false"), Edn::Bool(false));
}

#[test]
fn reads_positive_and_negative_ints() {
    assert_eq!(parse("42"), Edn::Int(42));
    assert_eq!(parse("-7"), Edn::Int(-7));
    assert_eq!(parse("+9"), Edn::Int(9));
}

#[test]
fn reads_float_with_exponent_and_decimal_marker() {
    assert_eq!(parse("3.5"), Edn::Float(3.5));
    assert_eq!(parse("1e3"), Edn::Float(1000.0));
    // Trailing BigDecimal `M` is stripped (documented precision-loss path).
    assert_eq!(parse("2.5M"), Edn::Float(2.5));
}

#[test]
fn reads_string_with_escapes() {
    assert_eq!(
        parse(r#""a\"b\\c\n\t\/""#),
        Edn::Str("a\"b\\c\n\t/".to_string())
    );
}

#[test]
fn reads_unicode_escape() {
    assert_eq!(parse(r#""A""#), Edn::Str("A".to_string()));
}

#[test]
fn reads_plain_keyword() {
    assert_eq!(parse(":type"), Edn::Keyword("type".to_string()));
}

#[test]
fn reads_namespaced_keyword() {
    assert_eq!(
        parse(":xtdb.api/valid-time"),
        Edn::Keyword("xtdb.api/valid-time".to_string())
    );
}

#[test]
fn reads_double_colon_keyword() {
    // `::xt/id` auto-resolves; we keep the ns/name text after stripping colons.
    assert_eq!(parse("::xt/id"), Edn::Keyword("xt/id".to_string()));
}

#[test]
fn reads_symbol() {
    assert_eq!(parse("foo-bar"), Edn::Symbol("foo-bar".to_string()));
}

// ---- Collections -----------------------------------------------------------

#[test]
fn reads_vector() {
    assert_eq!(
        parse("[1 2 3]"),
        Edn::Vector(vec![Edn::Int(1), Edn::Int(2), Edn::Int(3)])
    );
}

#[test]
fn reads_list() {
    assert_eq!(parse("(1 2)"), Edn::List(vec![Edn::Int(1), Edn::Int(2)]));
}

#[test]
fn reads_map_insertion_ordered() {
    let m = parse("{:a 1 :b 2}");
    assert_eq!(
        m,
        Edn::Map(vec![
            (Edn::Keyword("a".to_string()), Edn::Int(1)),
            (Edn::Keyword("b".to_string()), Edn::Int(2)),
        ])
    );
}

#[test]
fn reads_set() {
    assert_eq!(parse("#{1 2}"), Edn::Set(vec![Edn::Int(1), Edn::Int(2)]));
}

#[test]
fn commas_are_whitespace() {
    assert_eq!(
        parse("[1, 2, 3]"),
        Edn::Vector(vec![Edn::Int(1), Edn::Int(2), Edn::Int(3)])
    );
}

// ---- Tagged literals -------------------------------------------------------

#[test]
fn inst_resolves_to_epoch_micros() {
    // Cross-check against the shared valid-time parser.
    let expected = super::parse::parse_valid_time("2021-03-01T00:00:00Z")
        .unwrap()
        .wallclock();
    assert_eq!(
        parse(r#"#inst "2021-03-01T00:00:00Z""#),
        Edn::Inst(expected)
    );
}

#[test]
fn uuid_retains_string() {
    assert_eq!(
        parse(r#"#uuid "550e8400-e29b-41d4-a716-446655440000""#),
        Edn::Uuid("550e8400-e29b-41d4-a716-446655440000".to_string())
    );
}

#[test]
fn unknown_tag_is_tolerated_and_discarded() {
    // `#crux/id "x"` → the tag is dropped, the inner form retained.
    assert_eq!(parse(r#"#crux/id "abc""#), Edn::Str("abc".to_string()));
}

// ---- Comments & datum discard ----------------------------------------------

#[test]
fn line_comments_and_datum_discard_are_honored() {
    let v = parse("[1 ; drop me\n #_ 2 3]");
    assert_eq!(v, Edn::Vector(vec![Edn::Int(1), Edn::Int(3)]));
}

// ---- Top-level vector ------------------------------------------------------

#[test]
fn parse_top_vector_returns_elements() {
    let items = parse_top_vector("[{:a 1} {:b 2}]").expect("top vector");
    assert_eq!(items.len(), 2);
}

#[test]
fn parse_top_vector_rejects_non_vector() {
    let err = parse_top_vector("{:a 1}").unwrap_err();
    assert!(matches!(err, EdnError::Unsupported { .. }));
}

// ---- Adversarial: typed errors, never panics -------------------------------

#[test]
fn truncated_input_is_unexpected_eof() {
    let err =
        parse_top_vector("[{:xt/id :a :history [{:vt #inst \"2021-03-01T00:00:00Z\"").unwrap_err();
    assert_eq!(err, EdnError::UnexpectedEof);
}

#[test]
fn bad_inst_is_bad_tagged_literal() {
    let err = parse_one(r#"#inst "not-a-date""#).unwrap_err();
    assert!(matches!(err, EdnError::BadTaggedLiteral { ref tag, .. } if tag == "inst"));
}

#[test]
fn unbalanced_delimiter_is_reported() {
    let err = parse_top_vector("[{:xt/id :a} }").unwrap_err();
    assert!(matches!(err, EdnError::UnbalancedDelimiter { .. }));
}

#[test]
fn unterminated_string_is_reported() {
    let err = parse_one("\"Alice").unwrap_err();
    assert!(matches!(err, EdnError::UnterminatedString { .. }));
}

#[test]
fn huge_integer_is_typed_error_not_a_panic() {
    let big = "1".repeat(200);
    let err = parse_one(&big).unwrap_err();
    assert!(matches!(err, EdnError::BadNumber { .. }));
}

#[test]
fn ratio_is_unsupported() {
    let err = parse_one("1/2").unwrap_err();
    assert!(matches!(err, EdnError::Unsupported { ref construct, .. } if construct == "ratio"));
}

#[test]
fn regex_literal_is_unsupported() {
    let err = parse_one("#\"[0-9]+\"").unwrap_err();
    assert!(matches!(err, EdnError::Unsupported { .. }));
}

#[test]
fn eval_literal_is_unsupported() {
    let err = parse_one("#=(inc 1)").unwrap_err();
    assert!(
        matches!(err, EdnError::Unsupported { ref construct, .. } if construct == "eval literal")
    );
}

#[test]
fn deeply_nested_collection_is_too_deep_not_stack_overflow() {
    let depth = MAX_DEPTH + 50;
    let mut s = String::new();
    for _ in 0..depth {
        s.push('[');
    }
    for _ in 0..depth {
        s.push(']');
    }
    let err = parse_one(&s).unwrap_err();
    assert!(matches!(err, EdnError::TooDeep { .. }));
}

#[test]
fn datum_discard_chain_is_depth_bounded_not_stack_overflow() {
    // A long `#_#_#_…0` chain recurses once per discard; it must bail with a
    // typed TooDeep error, never overflow the stack.
    let s = format!("{}0", "#_".repeat(MAX_DEPTH + 50));
    let err = parse_one(&s).unwrap_err();
    assert!(matches!(err, EdnError::TooDeep { .. }), "got: {err}");
}

#[test]
fn error_carries_one_based_line_and_col() {
    // The bad character (a backtick, which starts no EDN form) is on line 3.
    let input = "[1\n 2\n `]";
    let err = parse_one(input).unwrap_err();
    let (line, _col) = err.position().expect("positional error");
    assert_eq!(line, 3, "error should be reported on line 3: {err}");
}

#[test]
fn trailing_data_after_one_value_is_rejected() {
    let err = parse_one("1 2").unwrap_err();
    assert!(matches!(err, EdnError::TrailingData { .. }));
}
