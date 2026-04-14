# ADR-0035: Mutation Testing with cargo-mutants

**Status:** Accepted
**Date:** 2026-02-07
**Deciders:** madmax983, Claude
**Categories:** Testing, CI/CD, Quality Assurance

## Context

AletheiaDB has 2,490 tests and 86.45% line coverage, meeting our quality thresholds. However, code coverage only measures whether code was *executed* during tests — it does not measure whether tests actually *assert correctness*. A test that calls a function but never checks its return value achieves 100% coverage while catching zero bugs.

Mutation testing addresses this gap by introducing small changes (mutants) to source code — flipping conditions, replacing return values, removing statements — and checking whether at least one test fails. A surviving mutant reveals a test gap: code that is executed but not meaningfully validated.

Key motivations:

1. **Test quality validation**: Coverage says "this code ran", mutation testing says "this code is actually tested"
2. **Correctness-critical domain**: Bi-temporal invariants, ACID guarantees, and graph consistency require tests that truly verify behavior, not just exercise paths
3. **Regression confidence**: Surviving mutants in WAL, MVCC, or temporal logic could mask real bugs
4. **CI integration need**: Must be practical for both PR feedback and comprehensive weekly analysis

## Decision

We will adopt **cargo-mutants** for mutation testing with a two-tier CI strategy:

### Tool Choice: cargo-mutants

We will use [cargo-mutants](https://mutants.rs/) as our mutation testing tool.

- Pure Rust, cargo-native — no JVM or external runtime
- Supports `--in-diff` for incremental testing of only changed code
- Supports `--shard` for parallelizing full runs across CI runners
- Configurable exclusions via `.cargo/mutants.toml`
- Active maintenance and Rust ecosystem alignment

### CI Strategy: Two Tiers

**Tier 1 — PR Diff (on every pull request):**
- Runs `cargo mutants --in-diff` against only the changed code
- Informational only (`continue-on-error: true`) — does not block merge
- Provides fast feedback on whether new/modified code has meaningful test coverage
- Results uploaded as artifacts and summarized in GitHub job summary

**Tier 2 — Full Sharded (weekly schedule + manual dispatch):**
- Runs full mutation testing sharded across 4 CI runners (`--shard k/4`)
- Comprehensive mutation score tracking for the entire codebase
- Per-shard artifacts uploaded for analysis
- Can be triggered manually via `workflow_dispatch`

### Exclusions

Low-value mutation targets are excluded via `.cargo/mutants.toml`:
- `benches/**`, `examples/**`, `tests/**` — not production code
- `src/mcp/**` — IO-heavy MCP server, validated via integration tests
- `Display`/`Debug` impls and `fmt` methods — formatting output rarely has correctness implications

### Local Recipes

Three `just` recipes for local mutation testing:
- `just mutants` — full run
- `just mutants-diff` — uncommitted changes only
- `just mutants-branch` — changes vs trunk

## Consequences

### Positive

- **Identifies weak tests**: Finds code where tests execute but don't assert, revealing false confidence from coverage metrics
- **Targeted PR feedback**: `--in-diff` keeps PR runs fast by only testing changed code
- **Scalable full analysis**: Sharding across 4 runners makes weekly full runs practical
- **Non-blocking adoption**: PR runs are informational, so adoption doesn't slow development velocity
- **Local developer workflow**: `just mutants-diff` enables pre-push mutation testing on changed code
- **Complements existing coverage**: Works alongside cargo-llvm-cov, not replacing it

### Negative

- **CI cost**: Weekly full runs consume significant runner minutes (4 parallel 90-minute jobs)
- **False positives**: Some surviving mutants are genuinely equivalent (e.g., changing `>=` to `>` in a context where equality never occurs) — requires triage
- **Slow local runs**: Full `just mutants` is impractical for quick iteration; use `mutants-diff` or `mutants-branch` instead
- **New tool dependency**: Adds cargo-mutants (via cargo-binstall) to CI toolchain

### Neutral

- **Informational-only on PRs**: Surviving mutants on PRs don't block merge — this is intentional during adoption but could be tightened later
- **Timeout tuning**: `timeout_multiplier = 3.0` may need adjustment as the test suite evolves
- **Exclusion scope**: MCP server exclusion may be revisited if integration test coverage improves

## Alternatives Considered

### Alternative 1: mutagen (Rust mutation testing)

An older Rust mutation testing tool using compiler plugin injection.

**Rejected because:**
- Requires nightly Rust compiler
- Less actively maintained than cargo-mutants
- No `--in-diff` support for incremental testing
- No built-in sharding for CI parallelization

### Alternative 2: Coverage-only (status quo)

Continue relying solely on line/function/region coverage thresholds.

**Rejected because:**
- Coverage does not validate assertion quality
- A test with zero assertions achieves full coverage while catching zero bugs
- For a database with ACID guarantees, "code was executed" is insufficient — "code was verified" is the bar

### Alternative 3: Blocking mutation testing on PRs

Require all mutants to be caught before merging.

**Rejected because:**
- Equivalent mutants create unavoidable false positives
- Would significantly slow PR velocity
- Better to adopt incrementally: informational first, then tighten thresholds as the team builds experience with triage
- Can revisit once baseline mutation score is established

### Alternative 4: Single full run (no sharding)

Run the full mutation test suite on a single runner.

**Rejected because:**
- Full runs on a large codebase easily exceed GitHub Actions' 6-hour job limit
- Sharding across 4 runners keeps each shard under 90 minutes
- Parallel execution provides results faster

## Implementation Notes

### Installation

cargo-mutants is installed via `cargo-binstall` in CI (pre-built binaries, no compilation):

```yaml
- uses: taiki-e/install-action@cargo-binstall
- run: cargo binstall --no-confirm cargo-mutants
```

Locally: `cargo install cargo-mutants` or `cargo binstall cargo-mutants`.

### Interpreting Results

Results are written to `mutants.out/` with these files:
- `caught.txt` — mutants killed by tests (good)
- `missed.txt` — mutants that survived (needs investigation)
- `timeout.txt` — mutants that caused test timeouts
- `unviable.txt` — mutants that failed to compile (neutral)

Focus on `missed.txt` — each entry indicates code where a behavioral change went undetected by tests.

### Future Considerations

- **Enforce mutation score**: Once a baseline is established, consider adding a minimum mutation score threshold
- **Per-module tracking**: Track mutation scores per module to identify weak areas
- **PR comments**: Add a bot that posts mutation results as PR comments for visibility
- **Revisit exclusions**: As MCP server tests mature, consider removing the `src/mcp/**` exclusion

## References

- PR #888: Implementation
- [cargo-mutants documentation](https://mutants.rs/)
- [Mutation Testing overview (Wikipedia)](https://en.wikipedia.org/wiki/Mutation_testing)
- ADR-0015: CI/CD Automation Workflow
- TESTING.md: Testing and coverage guide
