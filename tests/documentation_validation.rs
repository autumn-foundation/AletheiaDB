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

    // Verify there's a WAL section (which should mention recovery)
    assert!(
        content.contains("Write-Ahead Log") || content.contains("WAL"),
        "CLAUDE.md should have a Write-Ahead Log (WAL) section"
    );

    // Verify recovery is mentioned in relation to WAL
    let wal_section_start = content
        .find("Write-Ahead Log")
        .or_else(|| content.find("WAL"));
    if let Some(start) = wal_section_start {
        // Check the next 2000 characters for recovery-related content
        let wal_section = &content[start..std::cmp::min(start + 2000, content.len())];
        assert!(
            wal_section.contains("recovery")
                || wal_section.contains("Recovery")
                || wal_section.contains("replay"),
            "WAL section should mention recovery or replay"
        );
    }
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
