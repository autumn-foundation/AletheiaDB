# ADR-0063: Graph Context Exporter

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, LLM Integration

## Context

AletheiaDB provides powerful bi-temporal and semantic query capabilities. However, directly feeding raw graph data (JSON, triples, etc.) to an LLM for Retrieval-Augmented Generation (RAG) is highly inefficient in terms of tokens and difficult for models to effectively reason about.

We need a dedicated tool to convert a node's "ego network" (its identity, properties, temporal history, and immediate neighborhood) into a structured, human-readable format that an LLM can easily consume.

## Decision

We will implement the **Graph Context Exporter** ("The Scribe") as an experimental module in `src/experimental/graph_context.rs`.

The `GraphContextBuilder` generates rich, structured Markdown representations of a node that highlight:
1.  **Identity**: The node's ID and Label.
2.  **State**: Current properties and semantic vector summaries.
3.  **Evolution**: A narrative history of how the node has changed over time.
4.  **Context**: The node's structural neighborhood and relationships.

This component bridges the gap between the database's internal graph representations and external LLM interactions.

## Consequences

### Positive
-   **Enhanced RAG Performance**: Delivers a clear, contextual prompt to LLMs, improving their understanding and the quality of their generation.
-   **Token Efficiency**: Compresses raw database internals into a cleaner format, saving expensive LLM tokens.
-   **Standardized LLM Inputs**: Ensures consistency in how graph nodes are presented to AI models across different workflows.

### Negative
-   **Static Formatting**: The generated Markdown is a static representation; if the LLM needs a slightly different structure, the builder has to be modified.
-   **Loss of Exact Graph Details**: Converting to human-readable Markdown strips out exact structural properties (like raw vector floats), prioritizing narrative over exact data.

## References
- `src/experimental/graph_context.rs`
