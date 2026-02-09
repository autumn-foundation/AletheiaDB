# Verification Sweep Plan

This plan operationalizes a full correctness sweep using complementary techniques:

- Concurrency interleavings: Loom
- UB detection: Miri
- Test strength: cargo-mutants
- Input-space exploration: cargo-fuzz
- Bounded model checking: Kani
- Specification-level proofs: Verus
- Coverage depth: line/function/region and targeted MC/DC

## Goals

1. Keep fast PR feedback under ~15 minutes.
2. Run deeper nightly checks without blocking developers.
3. Run expensive weekly sweeps and track regressions over time.
4. Prioritize high-risk modules:
   - `src/storage/wal/*`
   - `src/index/vector/*`
   - `src/storage/historical/*`
   - parser/decoder paths

## Verification Matrix

| Layer | Tool | Cadence | Scope |
|---|---|---|---|
| Lint/style/build | `cargo fmt`, `clippy`, `test` | PR | whole workspace |
| Concurrency | Loom | PR + nightly | WAL/vector/temporal models |
| Coverage guardrail | `cargo llvm-cov` | PR | threshold check |
| UB/runtime model | Miri | nightly | library + concurrency-critical tests |
| Mutation testing | cargo-mutants | PR-diff + weekly full | changed code + full critical modules |
| Fuzzing | cargo-fuzz | nightly + weekly long | deserialization/parsers |
| BMC | Kani | nightly/weekly | bounded critical proofs |
| Formal proofs | Verus | weekly/manual gate | core invariants |

## Just Commands

Primary commands:

- `just verify-smoke`
- `just verify-nightly`
- `just verify-weekly`
- `just fuzz-setup`
- `just fuzz-smoke <target>`
- `just fuzz-run <target> <seconds>`
- `just kani-setup`
- `just kani`
- `just verus-setup`
- `just verus`

Windows note:

- If `just` has shell issues on your machine, run the underlying `cargo` commands directly.
- Example Loom run:
  - `cargo test --test loom_wal --test loom_vector --test loom_temporal`

## 4-Week Rollout

## Week 1: Baseline + CI Wiring

1. Enable PR checks:
   - `fmt-check`, `clippy -D warnings`, `cargo test`
   - Loom suite (`loom_wal`, `loom_vector`, `loom_temporal`)
2. Add nightly:
   - Loom suite
   - `cargo +nightly miri test --lib`
3. Keep existing PR-diff mutation check (`mutants-diff`).
4. Start metrics tracking:
   - pass/fail and duration
   - flaky count

Exit criteria:
- PR checks stable and deterministic for 7 consecutive days.

## Week 2: Fuzzing + Expanded Mutation

1. Initialize fuzzing:
   - `just fuzz-setup`
2. Add initial fuzz targets:
   - WAL entry decode path
   - WAL metadata decode path
   - vector mappings decode path
   - query/parser decode path
3. Nightly fuzz smoke:
   - 30-120s per target
4. Weekly full mutants sweep:
   - `cargo mutants --in-place -vV`

Exit criteria:
- At least 4 fuzz targets live, nightly smoke green, weekly mutant report generated.

## Week 3: Kani Proof Harnesses

1. Install Kani:
   - `just kani-setup`
2. Add bounded proof harnesses for:
   - LSN allocator monotonicity/uniqueness
   - ordering helper correctness
   - temporal ordering predicates
   - mapping consistency transitions
3. Run Kani nightly on harness subset; weekly full.

Exit criteria:
- 10+ critical Kani harnesses passing in CI.

## Week 4: Verus Core Invariants + MC/DC Hotspots

1. Add Verus proof modules for top invariants:
   - no temporal paradox predicates
   - monotonic timestamp/order properties
   - mapping coherence predicates
2. Add targeted MC/DC for safety-critical decision logic only.
3. Define release gate:
   - Loom + smoke tests + coverage thresholds + no critical mutant regressions.

Exit criteria:
- 3-5 high-value Verus invariants proven and documented.

## Invariant Backlog (Prioritized)

1. WAL:
   - global LSN order preserved after stripe drain+merge
   - no overlap in batch/single allocation
   - close/append race cannot publish after observed close
2. Vector:
   - no phantom vectors (inner present without mapping)
   - no zombie mappings (mapping without inner)
   - double-add winner/loser leaves coherent state
3. Temporal:
   - pre-anchor snapshot visibility before observer commit consumption
   - observer failure isolation
   - rotation/prune never removes current snapshot generation

## MC/DC Guidance

Use MC/DC only where branching is safety-critical and dense:

- WAL durability mode branching
- temporal paradox/validation branching
- mapping transition decision paths

Do not impose global MC/DC across all modules due cost/maintenance overhead.

## Artifacts and Reporting

Store periodic outputs:

- `mutants.out/` (weekly)
- fuzz corpus and crash artifacts (nightly/weekly)
- Kani/Verus reports under `docs/verification/`

Recommended dashboard metrics:

- pass rate and median runtime by stage
- mutant kill rate trend
- fuzz coverage growth and unique crash count
- proof count and status (Kani + Verus)

## Current Status (Implementation)

- Loom models: 17 (`tests/loom_wal.rs`, `tests/loom_vector.rs`, `tests/loom_temporal.rs`)
- Kani harnesses: 10 (`src/verification/kani_harnesses.rs`)
- Verus modules: 2
  - `verification/verus/temporal_invariants.rs`
  - `verification/verus/vector_mapping_invariants.rs`
- Fuzz targets: 4 (`fuzz/fuzz_targets/*.rs`)
- CI tiers wired: `.github/workflows/verification-tiers.yml`
  - PR: Loom subset + Kani smoke + Verus smoke + fuzz smoke
  - Nightly: full Loom + broader Kani (higher unwind) + longer fuzz
  - Weekly: mutants + Miri + extended fuzz with corpus minimization + Verus full core invariants
- MC/DC-style hotspot tests added for critical decision logic:
  - WAL durability mode branching (`src/storage/wal/concurrent_system.rs`)
  - Temporal range validation/paradox guards (`src/core/temporal.rs`)
  - Vector mapping transition classification (`src/index/vector/hnsw.rs`)

## Optional/Costly Tools

- TrustInSoft: evaluate only after OSS stack maturity; cost/benefit tends to be best once invariants and harnesses are already disciplined.
- Haybale/memscope-rs: use surgically for hard residual gaps, not as first-line continuous checks.
