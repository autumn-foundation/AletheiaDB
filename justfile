# GallifreyDB development commands
# Requires: cargo, cargo-llvm-cov, just
# Optional: Tracy profiler

# Default recipe - show available commands
default:
    @just --list

# Run all tests
test:
    cargo test

# Run tests with output
test-verbose:
    cargo test -- --nocapture --test-threads=1

# Run specific test
test-one TEST:
    cargo test {{TEST}} -- --nocapture

# Run benchmarks (when implemented)
bench:
    cargo bench

# Build the project
build:
    cargo build

# Build in release mode
build-release:
    cargo build --release

# Build with Tracy profiling enabled
build-tracy:
    cargo build --release --features tracy

# Check code without building
check:
    cargo check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting without modifying
fmt-check:
    cargo fmt -- --check

# Clean build artifacts
clean:
    cargo clean

# === Coverage Commands ===

# Run tests with coverage and generate HTML report
coverage:
    cargo llvm-cov --html --open

# Run coverage and check against thresholds
coverage-check:
    cargo llvm-cov --fail-under-lines 80

# Generate coverage report in lcov format (for CI)
coverage-ci:
    cargo llvm-cov --lcov --output-path lcov.info

# Generate coverage with detailed function-level report
coverage-detailed:
    cargo llvm-cov --html --open --show-missing-lines

# Show coverage summary in terminal
coverage-summary:
    cargo llvm-cov --summary-only

# === Profiling Commands ===

# Run with Tracy profiling (requires Tracy profiler to be running)
profile-tracy:
    @echo "Make sure Tracy profiler is running, then press Enter..."
    @pause
    cargo run --release --features tracy

# Run benchmarks with profiling
bench-profile:
    cargo bench --features tracy

# Profile a specific binary
profile-bin BIN:
    cargo run --release --features tracy --bin {{BIN}}

# === Development Workflow ===

# Full check: format, lint, test, coverage
check-all: fmt lint test coverage-check
    @echo "✓ All checks passed!"

# Pre-commit checks (fast)
pre-commit: fmt-check lint test
    @echo "✓ Pre-commit checks passed!"

# CI simulation - what runs in continuous integration
ci: fmt-check lint test coverage-ci
    @echo "✓ CI checks passed!"

# === Documentation ===

# Build and open documentation
doc:
    cargo doc --open --no-deps

# Build documentation with private items
doc-private:
    cargo doc --open --document-private-items

# Check documentation for broken links
doc-check:
    cargo doc --no-deps

# === Performance Testing ===

# Run criterion benchmarks (when implemented)
criterion:
    cargo bench --bench '*'

# Generate flamegraph (requires cargo-flamegraph)
flamegraph:
    cargo flamegraph --bench current_state

# === Maintenance ===

# Update dependencies
update:
    cargo update

# Check for outdated dependencies
outdated:
    cargo outdated

# Audit dependencies for security issues
audit:
    cargo audit

# === Git Worktree Commands ===
# These commands enable parallel development with multiple Claude instances

# Create new worktree with feature/fix branch
# Usage: just worktree-new feature/my-feature
worktree-new NAME:
    #!/usr/bin/env bash
    if command -v pwsh &> /dev/null; then
        pwsh -File scripts/worktree-new.ps1 {{NAME}}
    else
        bash scripts/worktree-new.sh {{NAME}}
    fi

# List all worktrees with status
worktree-list:
    #!/usr/bin/env bash
    if command -v pwsh &> /dev/null; then
        pwsh -File scripts/worktree-list.ps1
    else
        bash scripts/worktree-list.sh
    fi

# Remove worktree and clean up branches
# Usage: just worktree-remove feature/my-feature
worktree-remove NAME:
    #!/usr/bin/env bash
    if command -v pwsh &> /dev/null; then
        pwsh -File scripts/worktree-remove.ps1 {{NAME}}
    else
        bash scripts/worktree-remove.sh {{NAME}}
    fi

# Push current branch and create PR to trunk
# Usage: just worktree-pr "PR Title" "Optional description"
worktree-pr TITLE BODY="":
    #!/usr/bin/env bash
    if command -v pwsh &> /dev/null; then
        pwsh -File scripts/worktree-pr.ps1 "{{TITLE}}" "{{BODY}}"
    else
        bash scripts/worktree-pr.sh "{{TITLE}}" "{{BODY}}"
    fi
