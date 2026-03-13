# ADR-0064: Mosaic Semantic UI Rendering

**Status:** Proposed
**Date:** 2026-06-25
**Deciders:** Atlas, Codex
**Categories:** Experimental, Cognitive Architecture, Semantic Search

## Context

When building UI for semantic knowledge graphs, traditional generic table views or property inspectors are insufficient. The UI needs to dynamically adapt to the semantic type and context of the data being displayed. An entity representing a "Person" should be rendered differently than an entity representing a "Company" or a "Concept".

We need a UI rendering engine that is semantically aware.

## Decision

We will implement **Mosaic**, a Semantic UI Rendering Engine, as an experimental module in `src/experimental/mosaic.rs`.

The `Mosaic` engine works by inspecting the semantic embedding and properties of a node and selecting the most appropriate UI components (tiles or shards) to render its data. It allows for a dynamic, compositional UI where complex entities are built from reusable semantic components.

## Consequences

### Positive
-   **Dynamic User Experience**: The UI adapts to the data, providing a much richer experience than generic CRUD interfaces.
-   **Component Reusability**: UI components are tied to semantic concepts rather than hardcoded layouts, making the front-end highly modular.

### Negative
-   **Front-End Complexity**: Requires maintaining a registry of semantic UI components and mapping them to vector regions or ontologies.
-   **Performance Overhead**: Dynamically constructing UI layouts based on semantic inspection adds latency to the rendering pipeline.

## References
- `src/experimental/mosaic.rs`
