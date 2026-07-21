# Deferred MCP Registry Batch: 64 → 74 tools

**Date:** 2026-07-21
**Scope:** Register 10 designed-but-unregistered MCP tools and enroll
`get_belief_revisions` in the token budget, in one lockstep change.

## What

Four backing features landed their Rust APIs + tests with their MCP surfaces
explicitly **designed but deferred** to the coordinator-owned registry batch
(lane policy: one registry-changing PR at a time). This change wires those 10
tools into both MCP registries, bringing the advertised catalog from **64 to
74** tools, and enrolls the previously-omitted `get_belief_revisions` tool in
the Issue #3353 token-budget set.

### The 10 new tools

| Tool | Class | Feature gate | Backing PR |
|------|-------|--------------|-----------|
| `create_drift_monitor` | Write | `semantic-temporal` | #3728 (#3367) |
| `list_drift_monitors` | Read | `semantic-temporal` | #3728 |
| `delete_drift_monitor` | Write | `semantic-temporal` | #3728 |
| `query_drift_alarms` | Read | `semantic-temporal` | #3728 |
| `resolve_drift_alarm` | Write | `semantic-temporal` | #3728 |
| `contradiction_genealogy` | Read | `semantic-temporal` | #3742 (#3352) |
| `find_contradictions` | Read | `semantic-temporal` | #3742 |
| `counterfactual_replay` | Read | `semantic-temporal` | #3743 (#3357) |
| `trust_breakdown` | Read | `semantic-reasoning` | #3748 (#3382) |
| `list_trust_policies` | Read | `semantic-reasoning` | #3748 |

Class deltas: Read 46 → 53 (+7), Write 15 → 18 (+3), Metrics 1, Admin 2.
53 + 18 + 1 + 2 = **74**.

Group 5 (not a new tool): `get_belief_revisions` is added to
`BUDGETABLE_READ_TOOLS` — a count-neutral enrollment edit.

## Why

The four backing PRs deliberately left their MCP surfaces unregistered because
the tool registry is a single, lockstep-gated surface: every source-of-truth
(two class tables, two dispatch/route registries, the JSON inventory, three
golden arrays, the docs matrix, the completeness bench) must move together, so
registry changes are batched. This change is that batch.

## Feature-gating (feature-invariant count)

Each gated tool is advertised **unconditionally** in `tool_definitions()`
(Design A, the `get_belief_revisions` precedent from #3735), so the catalog is
exactly 74 under every feature combination (default / all-features /
no-default-features / single cohort). Each has:

- a `#[cfg(feature = "<cohort>")]` real handler that calls the backing Rust API
  and serializes its documented JSON shape, and
- a `#[cfg(not(feature = "<cohort>"))]` twin returning a structured
  `FAILED_PRECONDITION` with `{tool, required_feature}`.

Drift / contradiction / counterfactual gate on `semantic-temporal`; trust gates
on `semantic-reasoning`.

## Lockstep locations touched

Legacy `src/mcp/*`:
- `TOOL_ACCESS_CLASSES` (`auth.rs`), GOLDEN + count comments (`auth_tests.rs`).
- `tool_definitions()`, `dispatch_read_tool` arms, `handle_*` cfg-twin handlers,
  JSON serializers, `BUDGETABLE_READ_TOOLS` (`server.rs`).
- Request structs (`tools.rs`).
- Live-count assert + per-tool behavior tests (`tests.rs`).

`crates/aletheia-server/*`:
- `MCP_TOOL_CLASSES` (`security/rbac.rs`).
- HTTP handlers (`deferred_batch_tools.rs`), `app.rs` `routes![]`, `lib.rs`
  wiring, `edge_tools.rs` dispatch-routed consts.
- `full_surface_parity_sweep.rs` (rename `access_class_conformance_for_all_64`
  → `_74`, count asserts), `security_rbac.rs` count asserts.

Cross-cutting:
- `tests/parity/inventory.json` (count, `mcp.tools[]`, `budgetable_read_tools`).
- `tests/parity_mcp.rs` `TOOL_INVENTORY`.
- `docs/guides/access-control-matrix.md` rows.
- `benches/mcp_round_trip.rs` one scenario per new tool.
- `CLAUDE.md` tool tables.

## Deferred to a dedicated follow-up (NOT in this PR)

**Trust computed-confidence predicate params (#3748 AC7).** PR #3748 names "the
registry batch" as the home for its AC7 `ComputedConfidenceFilter` predicate
params and a `computed_confidence` read field. After review this was
**deliberately deferred** to its own follow-up PR: faithfully mirroring the
#3348 provenance-filter machinery for computed confidence requires a
filterable-tools registry + schema-param injection + **per-result trust
computation at the dispatch seam** (calling the semantic-reasoning-gated
`computed_confidence` for every returned entity to filter/annotate) + adding a
`computed_confidence` field to the read-response serialization of many
filterable read tools. That is a cross-cutting query/filter + read-path change,
not registry-registration work, and folding it in would balloon this batch and
collide with the read-path lanes. The two trust **tools**
(`trust_breakdown` / `list_trust_policies`) ARE fully registered here; only the
AC7 filter machinery + `computed_confidence` field are deferred. Memory key:
`aletheiadb-pr3775-trust-ac7-deferred`.

## Budget enrollment (Group 5, expanded)

`get_belief_revisions` plus the five new reader tools with a budgetable sibling
— `list_drift_monitors`, `query_drift_alarms`, `contradiction_genealogy`,
`find_contradictions`, `trust_breakdown` — are enrolled in
`BUDGETABLE_READ_TOOLS` (21 → 26). `counterfactual_replay` is **excluded** (its
AC8 `counterfactual: true` marker must never be stripped by budget-ladder
truncation) and `list_trust_policies` is excluded (small/bounded, like
`list_vector_indexes`). The two scan-heavy contradiction tools
(`contradiction_genealogy`, `find_contradictions`) are additionally enrolled in
`RESOURCE_LIMITED_READ_TOOLS` for wall-clock-timeout + byte-cap coverage.

## Acceptance criteria

- Catalog advertises exactly 74 tools under all feature combinations.
- Every new tool: feature-on happy path returns the documented shape;
  feature-off twin returns `FAILED_PRECONDITION` with `required_feature`;
  RBAC class matches the table above; appears in the inventory, both golden
  arrays, the docs matrix, and has a completeness-bench scenario.
- `counterfactual_replay` responses carry a `counterfactual: true` marker
  (PR #3743 AC8).
- `get_belief_revisions` honors `max_response_tokens` / `max_response_bytes`.
- All parity/golden/rbac/conformance suites green at 74.
</content>
