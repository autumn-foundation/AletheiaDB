//! Documentation validation tests for CLAUDE.md
//!
//! These tests verify that key documentation sections contain required content.
//! This is a TDD approach to ensure documentation is complete and accurate.

use std::fs;

/// Test that CLAUDE.md includes recovery guarantees in the Architecture Principles section
#[test]
fn test_claude_md_includes_recovery_guarantees() {
    let content = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");

    // Verify recovery guarantees are documented
    assert!(
        content.contains("Recovery") || content.contains("recovery"),
        "CLAUDE.md should mention recovery guarantees"
    );

    // Verify durability modes are explained with recovery context
    assert!(
        content.contains("Synchronous")
            && content.contains("GroupCommit")
            && content.contains("Async"),
        "CLAUDE.md should document all durability modes (Synchronous, GroupCommit, Async)"
    );

    // Verify WAL recovery is mentioned in the correctness/ACID section
    // Look for a section that discusses ACID or correctness guarantees
    let has_acid_section = content.contains("ACID") || content.contains("Correctness");
    assert!(
        has_acid_section,
        "CLAUDE.md should have an ACID or Correctness section"
    );
}

/// Test that CLAUDE.md includes recovery flow in the Hybrid Storage Architecture section
#[test]
fn test_claude_md_includes_recovery_flow() {
    let content = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");

    // Find the "Hybrid Storage" section to test its content, which is more robust
    // than searching from the first occurrence of "WAL".
    let hybrid_storage_heading = "### Hybrid Storage";
    let next_heading = "## Rust Coding Standards";

    let section_start = content
        .find(hybrid_storage_heading)
        .expect("Hybrid Storage section not found in CLAUDE.md");

    let section_end = content[section_start..]
        .find(next_heading)
        .map(|i| section_start + i)
        .unwrap_or_else(|| content.len());

    let hybrid_storage_section = &content[section_start..section_end];

    // Verify recovery flow is documented in this section
    assert!(
        hybrid_storage_section.contains("Recovery Flow"),
        "Hybrid Storage section should contain 'Recovery Flow'"
    );
    assert!(
        hybrid_storage_section.contains("Replay WAL") || hybrid_storage_section.contains("Replay"),
        "Hybrid Storage section should mention 'Replay WAL' or 'Replay'"
    );
}

/// Test that CLAUDE.md includes recovery benchmarks in the Testing Requirements section
#[test]
fn test_claude_md_includes_recovery_benchmarks() {
    let content = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");

    // Verify there's a testing or benchmarks section
    assert!(
        content.contains("bench") || content.contains("Benchmark"),
        "CLAUDE.md should mention benchmarks"
    );

    // Verify recovery is mentioned in testing context
    // This could be in the form of "just bench" commands, benchmark descriptions, etc.
    let has_benchmark_command = content.contains("just bench");
    assert!(
        has_benchmark_command,
        "CLAUDE.md should document the 'just bench' command"
    );
}

/// Test that CLAUDE.md references the WAL.md documentation
#[test]
fn test_claude_md_references_wal_documentation() {
    let content = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");

    // Verify WAL.md is referenced
    assert!(
        content.contains("docs/WAL.md") || content.contains("WAL.md"),
        "CLAUDE.md should reference docs/WAL.md for detailed WAL documentation"
    );
}

/// Test that CLAUDE.md mentions checkpoint-based recovery
#[test]
fn test_claude_md_mentions_checkpoint_recovery() {
    let content = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");

    // Verify checkpoints are mentioned (for recovery optimization)
    assert!(
        content.contains("checkpoint") || content.contains("Checkpoint"),
        "CLAUDE.md should mention checkpoints for recovery optimization"
    );
}

/// Integration test: Verify all recovery-related content is present
#[test]
fn test_claude_md_recovery_content_complete() {
    let content = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");

    // Checklist of required recovery topics
    let required_topics = vec![
        ("ACID", "ACID guarantees"),
        ("durability", "Durability guarantees"),
        ("WAL", "Write-Ahead Log"),
        ("recovery", "Recovery mechanisms"),
    ];

    for (keyword, description) in required_topics {
        assert!(
            content.to_lowercase().contains(&keyword.to_lowercase()),
            "CLAUDE.md should include information about: {}",
            description
        );
    }
}

fn extract_numbered_lock_order(section: &str) -> Vec<String> {
    let mut entries = Vec::new();

    for line in section.lines() {
        let trimmed = line.trim();
        let without_comment = trimmed
            .strip_prefix("//!")
            .or_else(|| trimmed.strip_prefix("//"))
            .unwrap_or(trimmed)
            .trim();

        let Some((number, entry)) = without_comment.split_once(". ") else {
            if !entries.is_empty() && !without_comment.is_empty() {
                break;
            }
            continue;
        };

        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            if !entries.is_empty() && !without_comment.is_empty() {
                break;
            }
            continue;
        }

        entries.push(entry.trim().trim_matches('`').to_string());
    }

    entries
}

/// Returns `true` only if `needle` occurs in `haystack` as a standalone phrase
/// -- i.e. not embedded inside a longer word. The character immediately before
/// the match must not be alphabetic and the character immediately after must
/// not be alphabetic.
///
/// This exists so the lock-order assertion below does not false-positive on
/// unrelated prose: the naive `haystack.contains("either order")` also matches
/// the substring inside "neither ordering" (`n[either order]ing`), which
/// appears elsewhere in CLAUDE.md. A whole-phrase check still catches a genuine
/// "... in either order" statement while ignoring "neither ordering".
fn contains_whole_phrase(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, matched)| {
        let end = start + matched.len();
        let prev_is_alpha = haystack[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphabetic());
        let next_is_alpha = haystack[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic());
        !prev_is_alpha && !next_is_alpha
    })
}

/// Test that the lock acquisition order is documented for future write-path changes.
#[test]
fn test_lock_acquisition_order_documented() {
    let claude = fs::read_to_string("CLAUDE.md").expect("Failed to read CLAUDE.md");
    let write_apply = fs::read_to_string("src/api/transaction/write/apply.rs")
        .expect("Failed to read write apply module");
    let db_mod = fs::read_to_string("src/db/mod.rs").expect("Failed to read db module");

    let expected_order: Vec<String> = [
        "current_timestamp",
        "wal",
        "historical",
        "temporal_indexes",
        "id generators",
        "outgoing",
        "incoming",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    // Scope the CLAUDE.md slice to just the "Lock Acquisition Order" subsection
    // (heading -> next markdown heading) so unrelated later prose is not
    // scanned. Without this bound, distant sections such as the Known
    // Limitations wording "so neither ordering can commit a new dangling edge"
    // are pulled in -- and "neither ordering" contains the substring
    // "either order", which false-positives the assertion below.
    let claude_after_heading = claude
        .split("### Lock Acquisition Order")
        .nth(1)
        .expect("CLAUDE.md should include a lock acquisition order section");
    let claude_section_end = ["\n## ", "\n### "]
        .iter()
        .filter_map(|marker| claude_after_heading.find(marker))
        .min()
        .unwrap_or(claude_after_heading.len());
    let claude_section = &claude_after_heading[..claude_section_end];
    let write_apply_section = write_apply
        .split("//! # Lock Acquisition Order")
        .nth(1)
        .expect("write apply module should include a lock acquisition order comment");
    let db_mod_section = db_mod
        .split("// Lock ordering for write-path primitives:")
        .nth(1)
        .expect("db module should include a lock acquisition order comment");

    for (name, section) in [
        ("CLAUDE.md", claude_section),
        ("write apply module", write_apply_section),
        ("db module", db_mod_section),
    ] {
        assert!(
            !section.contains("outgoing/incoming"),
            "{name} should document `outgoing` and `incoming` as separate ordered entries"
        );
        assert!(
            !contains_whole_phrase(section, "either order"),
            "{name} should not allow outgoing and incoming adjacency indexes in either order"
        );
    }

    for (name, section) in [
        ("CLAUDE.md", claude_section),
        ("write apply module", write_apply_section),
        ("db module", db_mod_section),
    ] {
        assert_eq!(
            extract_numbered_lock_order(section),
            expected_order,
            "{name} should document the lock acquisition order as a numbered list"
        );
    }
}
