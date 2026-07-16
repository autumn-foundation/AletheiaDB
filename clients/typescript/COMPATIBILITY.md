# Compatibility & Versioning Policy

`@aletheiadb/client` is a thin, typed wrapper over the AletheiaDB **HTTP API**. This document states which server versions an SDK version supports and how breaking HTTP changes propagate to SDK releases.

## Semantic versioning

The SDK follows [semver](https://semver.org/). Given `MAJOR.MINOR.PATCH`:

- **MAJOR** — a breaking change to the SDK's *public TypeScript surface* (a renamed/removed method, a changed request/response type, a changed error class). Dropping support for a server API version is also MAJOR.
- **MINOR** — additive, backward-compatible: wrapping a newly-merged endpoint (e.g. graduating a stub to a real call), new optional options, new exported types.
- **PATCH** — bug fixes and doc changes with no surface change.

## Server ↔ SDK support matrix

The SDK targets the **HTTP surface** described by `tests/parity/inventory.json` and the autumn-server route handlers (`crates/aletheia-server/src/*_tools.rs`). Because the server is pre-1.0 and its HTTP surface is being ported route-by-route, this initial SDK line tracks the surface by capability rather than a frozen server version.

| SDK version | Server HTTP surface | Notes |
|-------------|---------------------|-------|
| `0.1.x` | autumn-server node/edge/traverse/temporal + admin/health routes (Issue #3524 PR1–PR4) | 34 routes wrapped; vector/hybrid/query/schema/stats/batch/lineage were typed stubs (`NotImplementedError`). |
| `0.2.x` | full autumn-server surface — all 46 tools, including vector/hybrid/`query`/schema/stats/batch/lineage (Issue #3627) | Every stub graduated to a real typed call. `NotImplementedError` is retained (exported) but no longer thrown. HTTP error bodies use the unified nested envelope (Issue #3629). |

The remaining routes have merged: `findSimilar`, `hybridQuery`, `query`, `enableVectorIndex`, `listVectorIndexes`, `getSchema`, `databaseStats`, `temporalExtent`, `applyBatch`, `lineageUpstream`, and `lineageDownstream` are now real typed calls.

## How breaking HTTP changes propagate

The SDK mirrors the server's wire contract **exactly** (route paths, request/response field names, the `temporal` block, vector-elision descriptors, and the structured error envelope). When the server changes that contract:

1. **Additive server change** (new optional field, new route, new optional param) → **MINOR** SDK release. Existing calls are unaffected; new options/types are added.
2. **Breaking server change** (renamed/removed field, changed status/enum, changed error `code`) → **MAJOR** SDK release. The SDK pins the old shape until the major bump so an app is never silently mis-typed.
3. **New structured error `code`** → handled gracefully with **no** release required: an unknown `code` maps to the base `AletheiaError` with `retriable === false` (forward-compatible by design). A dedicated subclass for the new code is added in a **MINOR** release.
4. **Server gaps found while building the SDK** are filed against the server (per Issue #3369's out-of-scope note), **not** worked around client-side.

## Runtime support

- **Node.js ≥ 18** (global `fetch`). On older Node, inject a `fetch` implementation via `ClientOptions.fetch`.
- **Standard-`fetch` edge runtimes** (Vercel Edge, Cloudflare Workers, Deno) — the core client has no Node-only dependencies.
- Both **ESM** (`import`) and **CommonJS** (`require`) entry points are shipped and smoke-tested on every build.

## Deprecation policy

A deprecated method/type is marked with `@deprecated` (surfaced in editors) for at least one MINOR release before removal in the next MAJOR. Stubs that throw `NotImplementedError` are **not** considered deprecations — they are forward-declarations of endpoints that will become live.
