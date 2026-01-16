# GallifreyDB Development Workflow

This document describes the complete development workflow for GallifreyDB, including worktree management, quality checks, testing, profiling, and the feature development process.

## Table of Contents

- [Worktree-First Development](#worktree-first-development)
- [Pre-Commit Quality Checks](#pre-commit-quality-checks)
- [Feature Development Process](#feature-development-process)
- [Testing Requirements](#testing-requirements)
- [Code Review Checklist](#code-review-checklist)
- [Profiling and Performance Tools](#profiling-and-performance-tools)
- [Development Tools](#development-tools)

## Worktree-First Development

### ⚠️ CRITICAL: NEVER COMMIT DIRECTLY TO TRUNK

**TRUNK IS A PROTECTED BRANCH. YOU MUST ALWAYS USE WORKTREES AND PULL REQUESTS.**

Before making ANY code changes:
1. Check current branch: `git branch --show-current`
2. If on `trunk`, STOP and create a worktree: `just worktree-new feature/your-feature-name`
3. Work in the worktree, commit there, push, and create a PR
4. NEVER use `git commit` when on trunk - there is a pre-commit hook to prevent this

**The ONLY acceptable commits to trunk are automated merges from approved PRs.**

This is enforced by a pre-commit hook that will block direct commits to trunk.

### Worktree Workflow

**When starting ANY implementation task, you MUST:**

1. **Create a worktree first** before making any code changes:
   ```bash
   just worktree-new feature/descriptive-name   # For new features
   just worktree-new fix/descriptive-name       # For bug fixes
   ```

2. **Navigate to the worktree** and work there:
   ```bash
   cd agents/feature-descriptive-name
   ```

3. **After completing work**, commit, create PR, and clean up:
   ```bash
   git add . && git commit -m "feat: description"
   just worktree-pr "PR Title" "Description"
   # After merge: just worktree-remove feature/descriptive-name
   ```

This enables multiple Claude instances to work in parallel without conflicts. Each instance gets an isolated copy of the codebase.

**Skip worktree creation only if:**
- You're already in a worktree (check with `git worktree list`)
- The task is read-only (exploration, answering questions)
- The user explicitly asks you to work in the main repo

See `WORKTREE_WORKFLOW.md` for complete documentation.

## Pre-Commit Quality Checks

### ⚠️ MANDATORY: Pre-Commit Quality Checks

**BEFORE EVERY COMMIT, you MUST run these commands in order:**

```bash
# 1. Run clippy with ALL warnings as errors
cargo clippy --all-targets --all-features -- -D warnings

# 2. Format all code
cargo fmt --all

# 3. Verify tests pass
cargo test
```

**These checks are NON-NEGOTIABLE:**
- `cargo clippy` ensures code quality and catches potential bugs
- `cargo fmt` maintains consistent code style
- Both MUST pass before committing

### Recommended Workflow

```bash
# Make code changes
# ...

# Run quality checks
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all

# If clippy or fmt made changes, review them
git diff

# Run tests
cargo test

# Only then commit
git add .
git commit -m "feat: your change"
```

**Note:** The `just pre-commit` command includes these checks, but you should run them explicitly to see any issues immediately.

### Quick Commands

```bash
just pre-commit   # Run clippy, fmt, and test
just check-all    # Full quality check (includes coverage)
just lint         # Run clippy only
just fmt          # Format code only
```

## Feature Development Process

### 1. Design First

Document design in issue/PR description before writing code:
- Problem statement
- Proposed solution
- API surface
- Performance targets
- Test strategy

### 2. API Before Implementation

Define public API surface first:
- Function signatures
- Type definitions
- Error cases
- Documentation comments

**Example:**
```rust
/// Performs a hybrid graph+vector query combining traversal and similarity ranking.
///
/// # Arguments
/// * `start_node` - Starting node for traversal
/// * `edge_label` - Edge label to traverse
/// * `query_embedding` - Query vector for similarity ranking
/// * `k` - Number of top results to return
///
/// # Returns
/// Top k nodes ranked by similarity after graph traversal
///
/// # Errors
/// Returns `GraphError` if traversal fails or `VectorError` if ranking fails
pub fn traverse_and_rank(
    &self,
    start_node: NodeId,
    edge_label: &str,
    query_embedding: &[f32],
    k: usize,
) -> Result<Vec<(NodeId, f32)>, HybridQueryError> {
    todo!("Implementation follows test-driven development")
}
```

### 3. Test-Driven Development

Write tests before implementation:

```rust
#[test]
fn test_traverse_and_rank_basic() {
    let db = GallifreyDB::new();

    // Setup: create graph with embeddings
    let alice = db.create_node("Person", props! { "name" => "Alice" })?;
    let bob = db.create_node("Person", props! {
        "name" => "Bob",
        "embedding" => vec![0.1, 0.2, 0.3]
    })?;
    db.create_edge(alice, bob, "KNOWS", PropertyMap::new())?;

    // Test
    let query_emb = vec![0.1, 0.2, 0.3];
    let results = db.traverse_and_rank(alice, "KNOWS", &query_emb, 10)?;

    // Assert
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, bob);
    assert!(results[0].1 > 0.9);  // High similarity
}

#[test]
fn test_traverse_and_rank_no_embeddings() {
    // Test error case: nodes without embeddings
}

#[test]
fn test_traverse_and_rank_empty_traversal() {
    // Test edge case: no neighbors
}
```

### 4. Implement

Follow coding standards (see [CODING_STANDARDS.md](CODING_STANDARDS.md)):
- Strong typing (newtype wrappers for IDs)
- Comprehensive error handling (no unwrap/expect)
- Safety comments for unsafe code
- Performance considerations

### 5. Benchmark

Add benchmarks for performance-critical code:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_traverse_and_rank(c: &mut Criterion) {
    let db = setup_test_db(1000);  // 1000 nodes

    c.bench_function("traverse_and_rank_1k_nodes", |b| {
        b.iter(|| {
            db.traverse_and_rank(
                black_box(node_id),
                black_box("KNOWS"),
                black_box(&query_emb),
                black_box(10),
            )
        })
    });
}

criterion_group!(benches, bench_traverse_and_rank);
criterion_main!(benches);
```

### 6. Document

Update docs if architecture changes:
- ADRs for architectural decisions
- API documentation in code
- User guides for new features
- Update CLAUDE.md if workflow changes

## Testing Requirements

See [TESTING.md](../TESTING.md) for detailed testing instructions.

### Coverage Requirements

GallifreyDB enforces strict code coverage thresholds:
- **Minimum 85% line coverage** (current: 86.45%)
- **Minimum 88% function coverage** (current: 89.10%)
- **Minimum 88% region coverage** (current: 88.91%)

### Quick Commands

```bash
just test              # Run all tests
just coverage          # Generate HTML coverage report
just coverage-check    # Verify coverage meets thresholds
just bench             # Run benchmarks
just check-all         # Full quality check (tests, coverage, lint)
```

### Test Types Required

1. **Unit Tests**: Test each module in isolation
2. **Integration Tests**: End-to-end workflows
3. **Property-Based Tests**: Use `proptest` for temporal invariants
4. **Performance Benchmarks**: Required for critical paths

### Performance Benchmarks

**Required Benchmarks:**
1. Current-state single-hop traversal (<1µs)
2. Current-state 3-hop traversal (<100µs)
3. Time-travel reconstruction (<10ms)
4. Batch insertion throughput (>100k edges/sec)
5. Storage overhead (<2X vs non-temporal)

## Code Review Checklist

Before submitting a PR, verify:

- [ ] **Clippy passes**: `cargo clippy --all-targets --all-features -- -D warnings` with no errors
- [ ] **Code formatted**: `cargo fmt --all` applied
- [ ] **Tests pass**: All tests passing
- [ ] **Coverage maintained**: Coverage thresholds met
- [ ] Temporal invariants preserved
- [ ] No performance regression on benchmarks
- [ ] Error handling is comprehensive (no unwrap/expect)
- [ ] Tests cover edge cases
- [ ] Documentation updated
- [ ] No unsafe without safety comments
- [ ] Strong typing used (no raw primitives for IDs)
- [ ] Code follows [CODING_STANDARDS.md](CODING_STANDARDS.md)

## Profiling and Performance Tools

### Tracy Profiler

Use Tracy for detailed CPU profiling:

1. Download Tracy from [releases](https://github.com/wolfpld/tracy/releases)
2. Build with profiling: `cargo build --release --features tracy`
3. Run profiled build: `just profile-tracy`

**Instrumenting code:**
```rust
#[cfg(feature = "tracy")]
use tracy_client::span;

pub fn hot_path_function() {
    #[cfg(feature = "tracy")]
    let _span = span!("hot_path_function");
    // Function body
}
```

**Best Practices:**
- Instrument hot paths only (avoid overhead in cold paths)
- Use descriptive span names
- Profile release builds (optimizations enabled)
- Compare before/after for regression detection

### Criterion Benchmarks

Use Criterion for statistical benchmarking:

```bash
just bench                    # Run all benchmarks
cargo bench --bench my_bench  # Run specific benchmark
```

**Viewing Results:**
- HTML reports in `target/criterion/`
- Compare against baseline: `cargo bench --save-baseline my_baseline`
- Compare to baseline: `cargo bench --baseline my_baseline`

### Flamegraphs

Generate flamegraphs for performance analysis:

```bash
cargo flamegraph --bench my_bench -- --bench
```

## Development Tools

All common tasks via `just`:

### Essential Commands

```bash
just test              # Run all tests
just coverage          # Generate HTML coverage report
just lint              # Run clippy
just fmt               # Format code
just pre-commit        # Quick pre-commit checks
just check-all         # Full quality check
```

### Worktree Management

```bash
just worktree-new feature/name    # Create new worktree
just worktree-list                # List all worktrees
just worktree-remove feature/name # Remove worktree
just worktree-pr "Title" "Desc"   # Create PR from worktree
```

### Build & Run

```bash
just build             # Build debug
just build-release     # Build release
just run               # Run debug build
```

### Benchmarking & Profiling

```bash
just bench             # Run benchmarks
just profile-tracy     # Run with Tracy profiling
just flamegraph        # Generate flamegraph
```

### Documentation

```bash
just doc               # Generate docs
just doc-open          # Generate and open docs
```

### Clean

```bash
just clean             # Clean build artifacts
just clean-all         # Clean everything (including caches)
```

See `justfile` for complete list of commands.

## Git Commit Message Convention

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `refactor`: Code refactoring (no behavior change)
- `perf`: Performance improvement
- `test`: Adding or updating tests
- `chore`: Build process, tooling changes

**Examples:**
```
feat(vector): add multi-property vector index support

Implements ADR-0022 for multiple vector properties per database.
Each property gets an independent HNSW index.

Closes #389
```

```
fix(wal): prevent deadlock in group commit flush

The flush coordinator could deadlock when multiple stripes
filled simultaneously. Add timeout to prevent infinite wait.

Fixes #401
```

## Continuous Integration

GitHub Actions run on every PR:
- Clippy (all warnings as errors)
- Rustfmt check
- Test suite
- Coverage check
- Benchmark comparison (detect regressions)

**All checks must pass before merge.**

## Release Process

1. Update version in `Cargo.toml`
2. Update `CHANGELOG.md`
3. Create release PR
4. After merge, tag release: `git tag v0.x.0`
5. Push tag: `git push origin v0.x.0`
6. GitHub Actions builds and publishes to crates.io

## References

- [CODING_STANDARDS.md](CODING_STANDARDS.md) - Rust coding standards
- [TESTING.md](../TESTING.md) - Testing requirements and coverage
- [WORKTREE_WORKFLOW.md](../WORKTREE_WORKFLOW.md) - Detailed worktree workflow
- [Conventional Commits](https://www.conventionalcommits.org/) - Commit message format
