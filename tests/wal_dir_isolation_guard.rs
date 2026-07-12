//! Regression guard for Issue #3500 — shared relative `wal_dir` test cross-pollution.
//!
//! The default `WalConfig::wal_dir` is the CWD-relative path `"aletheiadb/wal"`
//! (`src/config.rs`). Any durable-config test that sets `wal_dir` (or a durable
//! `data_dir`) to a RELATIVE string literal instead of a per-test tempdir makes
//! every such test in one `cargo test` process write into the SAME directory.
//! Because DB startup eagerly decodes the entire WAL directory, a stray segment
//! from one test then corrupts a later test's startup with
//! `CorruptedData("Unknown WAL operation type: ...")` — an order-dependent flake.
//!
//! This meta-test scans the integration-test sources under `tests/` and fails if
//! any test assigns a relative string literal to `wal_dir`/`data_dir` (a builder
//! `.wal_dir("...")` / `.data_dir("...")` call, or a `wal_dir: "..."` /
//! `data_dir: PathBuf::from("...")` field). Tempdir-derived paths
//! (`temp.path().join("wal")`), variables, and absolute literals are allowed. Use
//! `aletheiadb::test_utils::unique_wal_dir()` for an isolated directory.
//!
//! There are ZERO offenders today (the historical offender, `redb_file_locking`,
//! was fixed in PR #3487); this guard keeps the footgun from silently returning.

use std::path::{Path, PathBuf};

/// Markers that introduce a WAL/data directory value.
const DIR_MARKERS: &[&str] = &[".wal_dir(", ".data_dir(", "wal_dir:", "data_dir:"];

/// A `PathBuf::from(...)` wrapper that may sit between a marker and a literal.
const PATHBUF_WRAP: &str = "PathBuf::from(";

/// Strip Rust line (`//`) and block (`/* */`) comments from source, preserving
/// newlines (so line numbers stay accurate) and respecting string literals (so a
/// `//` inside a `"..."` is not treated as a comment). This prevents false
/// positives on prose that merely mentions `wal_dir:`/`data_dir:` in a comment.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;

    enum S {
        Normal,
        Str,
        Line,
        Block,
    }
    let mut state = S::Normal;

    while i < b.len() {
        let c = b[i];
        let n = if i + 1 < b.len() { b[i + 1] } else { 0 };
        match state {
            S::Normal => {
                if c == b'"' {
                    state = S::Str;
                    out.push(c);
                    i += 1;
                } else if c == b'/' && n == b'/' {
                    state = S::Line;
                    i += 2;
                } else if c == b'/' && n == b'*' {
                    state = S::Block;
                    i += 2;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            S::Str => {
                if c == b'\\' {
                    // Preserve the escape and the escaped byte verbatim.
                    out.push(c);
                    if i + 1 < b.len() {
                        out.push(b[i + 1]);
                    }
                    i += 2;
                } else if c == b'"' {
                    state = S::Normal;
                    out.push(c);
                    i += 1;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            S::Line => {
                if c == b'\n' {
                    state = S::Normal;
                    out.push(b'\n');
                }
                i += 1;
            }
            S::Block => {
                if c == b'*' && n == b'/' {
                    state = S::Normal;
                    i += 2;
                } else {
                    if c == b'\n' {
                        out.push(b'\n');
                    }
                    i += 1;
                }
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// A relative literal is any bare string-literal path that is not absolute. The
/// #3500 footgun is specifically the CWD-relative default; absolute literals do
/// not share a directory across tests via the CWD.
fn is_relative_literal(lit: &str) -> bool {
    !lit.starts_with('/')
}

/// Scan source text and return `(line_number, snippet)` for each `wal_dir`/
/// `data_dir` assigned to a RELATIVE string literal. Comments are stripped first.
fn find_relative_dir_literals(src: &str) -> Vec<(usize, String)> {
    let src = strip_comments(src);
    let bytes = src.as_bytes();
    let mut offenders = Vec::new();

    for marker in DIR_MARKERS {
        let mut search_from = 0;
        while let Some(rel) = src[search_from..].find(marker) {
            let start = search_from + rel;
            let after = start + marker.len();
            search_from = after;

            // Skip whitespace after the marker.
            let mut i = after;
            while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                i += 1;
            }

            // Optionally step over a `PathBuf::from(` wrapper (and its whitespace).
            if src[i..].starts_with(PATHBUF_WRAP) {
                i += PATHBUF_WRAP.len();
                while i < bytes.len() && (bytes[i] as char).is_whitespace() {
                    i += 1;
                }
            }

            // The value must be a string literal to be a static relative path.
            // Anything else (a variable, `temp.path().join(...)`, etc.) is fine.
            if i < bytes.len() && bytes[i] == b'"' {
                let lit_start = i + 1;
                if let Some(end_rel) = src[lit_start..].find('"') {
                    let literal = &src[lit_start..lit_start + end_rel];
                    if is_relative_literal(literal) {
                        let line = src[..start].bytes().filter(|&b| b == b'\n').count() + 1;
                        offenders.push((line, format!("{}\"{}\"", marker, literal)));
                    }
                }
            }
        }
    }

    offenders
}

/// Recursively collect `*.rs` files under `dir`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// T3 (guard self-test): the detector must FLAG the anti-pattern and NOT flag
/// legitimate isolated forms. A guard that always passes proves nothing.
#[test]
fn test_detector_flags_bad_and_ignores_good() {
    // BAD — relative string literal assigned to wal_dir/data_dir (the #3500 footgun).
    let bad_builder = r#"let c = WalConfigBuilder::new().wal_dir("aletheiadb/wal").build();"#;
    let bad_field = r#"let c = WalConfig { wal_dir: "wal".into(), ..Default::default() };"#;
    let bad_pathbuf = r#"let c = WalConfig { data_dir: PathBuf::from("data/x") };"#;
    let bad_dot_rel = r#"let c = WalConfigBuilder::new().wal_dir("./aletheiadb/wal").build();"#;

    assert!(
        !find_relative_dir_literals(bad_builder).is_empty(),
        "must flag a relative .wal_dir(\"...\") builder literal"
    );
    assert!(
        !find_relative_dir_literals(bad_field).is_empty(),
        "must flag a relative wal_dir: \"...\" field literal"
    );
    assert!(
        !find_relative_dir_literals(bad_pathbuf).is_empty(),
        "must flag a relative data_dir: PathBuf::from(\"...\") literal"
    );
    assert!(
        !find_relative_dir_literals(bad_dot_rel).is_empty(),
        "must flag a relative ./-prefixed wal_dir literal"
    );

    // GOOD — tempdir-derived paths, variables, and absolute literals.
    let good_builder =
        r#"let c = WalConfigBuilder::new().wal_dir(temp.path().join("wal")).build();"#;
    let good_var = r#"let c = WalConfigBuilder::new().wal_dir(wal_dir.clone()).build();"#;
    let good_field = r#"let c = WalConfig { data_dir: temp_dir.path().join("data") };"#;
    let good_absolute =
        r#"let c = WalConfigBuilder::new().wal_dir("/tmp/isolated-xyz/wal").build();"#;
    let good_type_annotation = r#"let wal_dir: PathBuf = temp.path().join("wal");"#;

    assert!(
        find_relative_dir_literals(good_builder).is_empty(),
        "must NOT flag a tempdir .join(\"wal\") builder path"
    );
    assert!(
        find_relative_dir_literals(good_var).is_empty(),
        "must NOT flag a variable wal_dir argument"
    );
    assert!(
        find_relative_dir_literals(good_field).is_empty(),
        "must NOT flag a tempdir .join(\"data\") field path"
    );
    assert!(
        find_relative_dir_literals(good_absolute).is_empty(),
        "must NOT flag an absolute wal_dir literal"
    );
    assert!(
        find_relative_dir_literals(good_type_annotation).is_empty(),
        "must NOT flag a `let wal_dir: PathBuf = ...` type annotation"
    );

    // GOOD — a comment that merely mentions the footgun must be ignored (this is
    // the exact shape of tests/regressions/persistence_default_isolation.rs).
    let good_line_comment = r#"// historically data_dir: "data" was the default footgun"#;
    let good_doc_comment = "//! Before the fix, the default was `data_dir: \"data\"`, so\n";
    assert!(
        find_relative_dir_literals(good_line_comment).is_empty(),
        "must NOT flag a wal_dir/data_dir mention inside a line comment"
    );
    assert!(
        find_relative_dir_literals(good_doc_comment).is_empty(),
        "must NOT flag a data_dir literal inside a doc comment"
    );
}

/// T4: the guard passes over the real current test tree (0 offenders).
#[test]
fn test_no_test_uses_relative_shared_wal_dir() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let tests_dir = Path::new(manifest).join("tests");

    let mut files = Vec::new();
    collect_rs_files(&tests_dir, &mut files);
    assert!(
        !files.is_empty(),
        "expected to find test source files under {}",
        tests_dir.display()
    );

    // This guard's own fixtures contain deliberate bad snippets; do not scan it.
    let this_file = "wal_dir_isolation_guard.rs";

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        if file.file_name().and_then(|n| n.to_str()) == Some(this_file) {
            continue;
        }
        let src = std::fs::read_to_string(file).unwrap_or_default();
        for (line, snippet) in find_relative_dir_literals(&src) {
            offenders.push(format!("{}:{} -> {}", file.display(), line, snippet));
        }
    }

    assert!(
        offenders.is_empty(),
        "Found test(s) assigning a relative/shared wal_dir or data_dir string literal instead \
         of a per-test tempdir (Issue #3500). This can cross-pollute other tests via the eager \
         full-directory WAL decode. Use aletheiadb::test_utils::unique_wal_dir() and pass \
         `dir.path().join(\"wal\")`. Offenders:\n{}",
        offenders.join("\n")
    );
}
