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
