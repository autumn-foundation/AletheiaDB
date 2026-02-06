# ADR-0030: Adopt Model Context Protocol (MCP)

**Status:** Accepted
**Date:** 2026-05-24
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, interface, ai-integration

## Context

AletheiaDB's primary use case is enabling "Reasoning LLMs" to query knowledge graphs with temporal context.
Currently, integrating AletheiaDB with an LLM agent (like Claude, ChatGPT, or custom LangChain agents) requires developers to write custom glue code:
1.  Define tool schemas (JSON) manually.
2.  Implement a REST or Python wrapper to translate tool calls to AletheiaDB API calls.
3.  Manage the context window and error handling manually.

This friction contradicts our "DX First" and "Agent-Native" philosophy.
The industry is converging on the **Model Context Protocol (MCP)** as a standard for exposing tools and resources to AI agents.

## Decision

We will implement a **Native MCP Server** directly within the AletheiaDB codebase (`src/mcp`).

The implementation will:
1.  **Be Built-in**: Not a separate sidecar, but a feature of the main binary (or a dedicated `aletheia-mcp` binary sharing the same core).
2.  **Transport**: Support `stdio` (Standard Input/Output) primarily, allowing local agents (like Cursor, Claude Desktop) to spawn the process directly.
3.  **Tool Exposure**: Automatically map core Graph, Vector, and Temporal operations to MCP Tools:
    *   `query_graph` (Cypher/AQL)
    *   `find_similar_vectors`
    *   `get_node_history`
4.  **Resources**: Expose nodes and relationships as MCP Resources (URI-addressable).

## Consequences

### Positive

*   **Zero-Config Integration**: Users can add AletheiaDB to Claude/Cursor config by simply pointing to the binary. No Python glue code needed.
*   **Standardized AI Interface**: Decouples the "AI Tooling" interface from the Rust/HTTP API. We can optimize the tool descriptions (prompts) specifically for LLM reasoning without affecting the programmatic API.
*   **Discovery**: Agents can dynamically discover the capabilities of the database (e.g., which vector indexes exist).

### Negative

*   **Maintenance Burden**: We must maintain the tool schemas and ensure they align with the underlying engine capabilities.
*   **Protocol Churn**: MCP is a relatively new standard; breaking changes in the protocol may require updates to our server.
*   **Security Surface**: Exposing a stdio interface that accepts JSON commands requires strict input validation (handled by the `Warden` persona/module) to prevent command injection or DoS.

## Implementation Notes

*   The MCP server should use `tokio` for async I/O.
*   Tool schemas should be strictly typed using `serde` and `schemars` to ensure valid JSON-Schema generation.
*   Long-running queries must be handled carefully to avoid timing out the agent's context window.
