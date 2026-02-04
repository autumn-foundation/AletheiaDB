# ADR-0031: Internalize Honeycomb Client

**Status:** Proposed
**Date:** 2026-05-24
**Deciders:** GallifreyDB Core Team
**Categories:** engineering, observability, dependency-management

## Context

Observability is a critical requirement for GallifreyDB ("Warden" and "Havoc" personas rely on it). We use Honeycomb.io as our primary observability backend.
Previously, the project relied on the `libhoney-rust` crate. However, this crate:
1.  Is effectively unmaintained.
2.  Required a `git` dependency in `Cargo.toml`, which breaks `crates.io` publishing and introduces supply chain risks.
3.  Had outdated dependencies (e.g., old `reqwest` or `openssl` versions) causing diamond dependency conflicts.

## Decision

We have replaced the external `libhoney-rust` dependency with a **Custom, In-Tree Client** located in `src/honeycomb`.

This module (`src/honeycomb`) is a clean-room implementation that:
1.  **Focuses on Minimalism**: Implements only the subset of the Honeycomb API required for `tracing` integration (Event submission, Batching).
2.  **Modern Stack**: Uses `reqwest` (async) and `tokio` natively, aligning with the rest of GallifreyDB's async runtime.
3.  **Type Safety**: Enforces strict typing for datasets and API keys to prevent misconfiguration.

## Consequences

### Positive

*   **Supply Chain Security**: We no longer depend on an unverified git repository. The telemetry code is scanned by our internal linters and security checks (`cargo audit`).
*   **Build Stability**: Removes the flakiness associated with git dependencies (network issues, commit hash changes).
*   **Dependency Alignment**: We share the HTTP client (`reqwest`) with other modules, reducing binary size and compile time compared to pulling in a separate HTTP client for the library.

### Negative

*   **Maintenance Overhead**: The team is now responsible for maintaining the Honeycomb client logic (retries, backoff, batching efficiency).
*   **Feature Gaps**: If Honeycomb releases new API features (e.g., new compression formats), we must manually implement them.

## Implementation Details

The implementation includes:
*   `Client`: The main entry point.
*   `BatchBuffer`: Aggregates events to reduce HTTP overhead.
*   `Transmission`: Handles the actual HTTP POST requests to `api.honeycomb.io`.

It is designed to be API-compatible enough to drop into our existing `tracing` setup with minimal changes.
