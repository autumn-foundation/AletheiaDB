//! Tests to verify Architecture Decision Records (ADRs) are properly documented.
//!
//! These tests ensure that ADRs contain all required sections and accurately
//! reflect the actual implementation decisions in the codebase.

use std::fs;
use std::path::Path;

/// Test that ADR-0009 (Strong ID Types) documents the memory ordering decision.
///
/// Related: Issue #199 - The ADR should explain why `Ordering::SeqCst` is used
/// instead of `Ordering::Relaxed` for ID generation, and how this relates to
/// snapshot isolation guarantees.
#[test]
fn test_adr_0009_documents_memory_ordering() {
    let adr_path = Path::new("docs/adr/0009-strong-id-types.md");

    // Verify the ADR file exists
    assert!(
        adr_path.exists(),
        "ADR-0009 file not found at {:?}",
        adr_path
    );

    let content = fs::read_to_string(adr_path).expect("Failed to read ADR-0009");

    // The ADR should have a "Memory Ordering" section
    assert!(
        content.contains("## Memory Ordering") || content.contains("### Memory Ordering"),
        "ADR-0009 should contain a 'Memory Ordering' section explaining the SeqCst decision"
    );

    // The ADR should explain why SeqCst is used
    assert!(
        content.contains("SeqCst") || content.contains("sequentially consistent"),
        "ADR-0009 should explain the use of SeqCst ordering"
    );

    // The ADR should contrast with Relaxed ordering
    assert!(
        content.contains("Relaxed"),
        "ADR-0009 should explain why Relaxed ordering is insufficient"
    );

    // The ADR should mention snapshot isolation as the reason
    assert!(
        content.contains("snapshot isolation")
            || content.contains("visibility")
            || content.contains("cross-thread"),
        "ADR-0009 should explain the connection to snapshot isolation or cross-thread visibility"
    );

    // The ADR should discuss the performance trade-off
    assert!(
        content.contains("performance") || content.contains("overhead") || content.contains("cost"),
        "ADR-0009 should discuss the performance implications of SeqCst"
    );

    // Code examples should use SeqCst, not Relaxed (issue #199 fix)
    // Count occurrences to ensure examples have been updated
    let seqcst_count = content.matches("Ordering::SeqCst").count();

    assert!(
        seqcst_count > 0,
        "ADR-0009 code examples should demonstrate Ordering::SeqCst usage"
    );

    // The main ID generation examples should use SeqCst, not Relaxed
    // Check that the IdGenerator implementation section (before Memory Ordering) uses SeqCst
    if let Some(id_gen_section_start) = content.find("### ID Generation") {
        // Find the end of the ID Generation section (start of Memory Ordering section)
        let section_end = content[id_gen_section_start..]
            .find("### Memory Ordering")
            .map(|offset| id_gen_section_start + offset)
            .unwrap_or_else(|| content.len());

        let id_gen_section = &content[id_gen_section_start..section_end];

        // In the ID Generation section, there should be no Relaxed ordering in the main implementation
        let has_relaxed_in_main_impl = id_gen_section.contains("pub fn next_node_id")
            && id_gen_section.contains("fetch_add(1, Ordering::Relaxed)");

        assert!(
            !has_relaxed_in_main_impl,
            "The main ID generation implementation should use SeqCst, not Relaxed"
        );
    }

    // It's acceptable to show Relaxed in "alternatives" or "incorrect example" sections
    // as long as they're clearly marked as such (e.g., with INCORRECT or NOT SUFFICIENT comments)
}

/// Test that ADR-0009 code examples align with actual implementation in src/core/id.rs
#[test]
fn test_adr_0009_matches_implementation() {
    let adr_path = Path::new("docs/adr/0009-strong-id-types.md");
    let impl_path = Path::new("src/core/id.rs");

    assert!(adr_path.exists(), "ADR-0009 not found");
    assert!(impl_path.exists(), "src/core/id.rs not found");

    let adr_content = fs::read_to_string(adr_path).expect("Failed to read ADR-0009");
    let impl_content = fs::read_to_string(impl_path).expect("Failed to read src/core/id.rs");

    // Implementation uses SeqCst
    assert!(
        impl_content.contains("fetch_add(1, Ordering::SeqCst)"),
        "Implementation in src/core/id.rs should use Ordering::SeqCst"
    );

    // ADR should use SeqCst in the main implementation examples
    // Check that the main IdGenerator implementation section uses SeqCst
    if let Some(id_gen_start) = adr_content.find("### ID Generation")
        && let Some(memory_ordering_start) = adr_content.find("### Memory Ordering")
    {
        let id_gen_section = &adr_content[id_gen_start..memory_ordering_start];

        // The main implementation should use SeqCst, not Relaxed
        if id_gen_section.contains("pub fn next") && id_gen_section.contains("fetch_add") {
            assert!(
                !id_gen_section.contains("Ordering::Relaxed"),
                "Main ID generation implementation in ADR should use SeqCst, not Relaxed"
            );
            assert!(
                id_gen_section.contains("Ordering::SeqCst"),
                "Main ID generation implementation in ADR should use SeqCst"
            );
        }
    }
}

/// Test that ADR-0009 references the correct related issues and ADRs
#[test]
fn test_adr_0009_references() {
    let adr_path = Path::new("docs/adr/0009-strong-id-types.md");
    let content = fs::read_to_string(adr_path).expect("Failed to read ADR-0009");

    // Should reference ADR-0003 (MVCC with Snapshot Isolation)
    assert!(
        content.contains("ADR-0003") || content.contains("0003"),
        "ADR-0009 should reference ADR-0003 for MVCC/snapshot isolation context"
    );

    // After fix, should reference issue #21 regarding SeqCst discussion
    // (mentioned in the actual implementation comments)
    // This is optional but recommended for traceability
}
