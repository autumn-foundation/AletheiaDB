//! Documentation acceptance for daemon-owned MCP (Issue #2905).
//!
//! The issue asks specifically for "a Windows/Codex-style setup showing one
//! shared daemon and MCP clients using it". Documentation is an acceptance
//! criterion here, not a nicety: without it the operator falls back to the
//! stopgap (every MCP process opening the same data directory) that the issue
//! exists to retire. These tests pin the parts an operator must be able to find.

use std::fs;

const GUIDE: &str = "docs/guides/daemon-mode.md";

fn guide() -> String {
    fs::read_to_string(GUIDE).unwrap_or_else(|e| panic!("failed to read {GUIDE}: {e}"))
}

/// The guide exists and explains the ownership boundary the daemon establishes.
#[test]
fn daemon_guide_explains_the_single_owner_boundary() {
    let content = guide();
    assert!(
        content.contains("aletheia daemon start"),
        "the guide documents how to start the daemon"
    );
    assert!(
        content.contains("ALETHEIADB_DAEMON_URL"),
        "the guide documents the daemon-client env var"
    );
    assert!(
        content.to_lowercase().contains("does not open")
            || content.to_lowercase().contains("never opens"),
        "the guide states that daemon-mode clients do not open local storage"
    );
}

/// A Windows/Codex-style setup is present, with a copy-pasteable client config.
#[test]
fn daemon_guide_has_a_windows_codex_setup() {
    let content = guide();
    assert!(
        content.contains("Windows"),
        "the guide has a Windows section"
    );
    assert!(
        content.to_lowercase().contains("codex"),
        "the guide shows a Codex-style MCP client setup"
    );
    assert!(
        content.contains("os error 5") || content.contains("Access is denied"),
        "the guide explains the Windows upgrade friction the daemon removes"
    );
}

/// Checks that need the proxy's own constants, so they can only compile when the
/// MCP surface is. Gated as a module (rather than as `#![cfg]` on the file) so
/// the feature-independent documentation assertions above keep running in every
/// build — including the `just check-features` matrix that caught this.
#[cfg(feature = "mcp-server")]
mod proxy_constants {
    use super::guide;

    /// The client-configuration blocks must be **correct**, not merely present:
    /// they are copy-pasted verbatim, and a wrong env-var name silently leaves every
    /// client in embedded mode — the divergent-database failure this feature exists
    /// to retire. Parsed and checked against the constants the binary actually reads.
    #[test]
    fn the_documented_client_config_names_the_env_var_the_binary_reads() {
        let content = guide();

        let blocks: Vec<String> = fenced_block(&content, "json")
            .into_iter()
            .filter(|block| block.contains("mcpServers"))
            .collect();
        assert!(
            !blocks.is_empty(),
            "the guide has at least one JSON mcpServers block"
        );

        // EVERY documented block must be correct — a reader copies whichever one
        // they reach first.
        for block in &blocks {
            let parsed: serde_json::Value = serde_json::from_str(block).unwrap_or_else(|e| {
                panic!("a documented client config is not valid JSON ({e}): {block}")
            });
            let server = find_mcp_server(&parsed)
                .unwrap_or_else(|| panic!("no mcpServers.aletheiadb entry in: {parsed}"));
            assert!(
                server["command"]
                    .as_str()
                    .is_some_and(|command| command.contains("aletheia-mcp")),
                "the documented command runs the proxy binary: {parsed}"
            );
            assert!(
                server["env"]
                    .get(aletheiadb::mcp::proxy::DAEMON_URL_ENV)
                    .is_some(),
                "the documented config sets {} — the variable the binary reads: {parsed}",
                aletheiadb::mcp::proxy::DAEMON_URL_ENV
            );
        }

        // The Codex block is TOML, checked the same way by key presence.
        let toml_block = fenced_block(&content, "toml")
            .into_iter()
            .find(|block| block.contains("mcp_servers"))
            .expect("the guide has a Codex TOML block");
        assert!(
            toml_block.contains(aletheiadb::mcp::proxy::DAEMON_URL_ENV),
            "the Codex config sets {}: {toml_block}",
            aletheiadb::mcp::proxy::DAEMON_URL_ENV
        );
    }

    /// Locate the `mcpServers.aletheiadb` entry, at the document root or nested
    /// under `mcp_client_config` (the shape `aletheia daemon status --json` emits).
    fn find_mcp_server(value: &serde_json::Value) -> Option<&serde_json::Value> {
        value
            .get("mcpServers")
            .or_else(|| value.get("mcp_client_config")?.get("mcpServers"))?
            .get("aletheiadb")
    }

    /// Extract the bodies of fenced code blocks with the given language tag.
    fn fenced_block(content: &str, language: &str) -> Vec<String> {
        let fence = format!("```{language}");
        let mut blocks = Vec::new();
        let mut rest = content;
        while let Some(start) = rest.find(&fence) {
            let after = &rest[start + fence.len()..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            match after.find("```") {
                Some(end) => {
                    blocks.push(after[..end].to_string());
                    rest = &after[end + 3..];
                }
                None => break,
            }
        }
        blocks
    }
}

/// Shutdown semantics are explicit (an acceptance criterion in its own right).
#[test]
fn daemon_guide_documents_shutdown_semantics() {
    let content = guide().to_lowercase();
    assert!(
        content.contains("aletheia daemon stop"),
        "the guide documents how to stop the daemon"
    );
    assert!(
        content.contains("disconnect"),
        "the guide explains that proxy processes exit when the client disconnects"
    );
    assert!(
        content.contains("long-lived") || content.contains("long lived"),
        "the guide states that the daemon is the long-lived process"
    );
}

/// The embedded fallback stays documented — it is still supported.
#[test]
fn daemon_guide_documents_the_embedded_fallback() {
    let content = guide();
    assert!(
        content.contains("embedded"),
        "the guide documents the embedded (no-daemon) fallback mode"
    );
}

/// CLAUDE.md points at the guide, so the architecture summary stays in sync.
#[test]
fn claude_md_links_the_daemon_guide() {
    let content = fs::read_to_string("CLAUDE.md").expect("read CLAUDE.md");
    assert!(
        content.contains("docs/guides/daemon-mode.md"),
        "CLAUDE.md links the daemon-mode guide"
    );
    assert!(
        content.contains("ALETHEIADB_DAEMON_URL"),
        "CLAUDE.md mentions the daemon-client env var"
    );
}
