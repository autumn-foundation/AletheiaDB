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

# Run benchmarks
bench:
    cargo bench

# Run benchmarks and generate HTML tables
bench-tables:
    cargo bench --all-features
    python scripts/generate_benchmark_tables.py
    @echo "✓ Benchmark tables generated in benchmark-results/"
    @echo "  Open benchmark-results/index.html to view results"

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
    cargo clippy --all-targets --all-features -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Clean build artifacts
clean:
    cargo clean

# === Coverage Commands ===

# Run tests with coverage and generate HTML report
coverage:
    cargo llvm-cov --html --open

# Run coverage and check against thresholds
coverage-check:
    cargo llvm-cov --all-features --fail-under-lines 85 --fail-under-functions 88 --fail-under-regions 88

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

# Run profiling benchmark with Tracy (open Tracy GUI first)
profile-commit:
    @echo "Ensure Tracy profiler is running, then press Enter..."
    @pause
    cargo bench --bench profiling_commit --features tracy -- --profile-time 10

# Run profiling with observability-tracy bridge
profile-commit-tracing:
    cargo bench --bench profiling_commit --features observability-tracy

# Quick profiling check (single scenario)
profile-quick:
    cargo bench --bench profiling_commit --features tracy -- sequential/ops/10

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

# === Changelog Management ===

# Generate changelog for unreleased changes
changelog:
    git-cliff --config cliff.toml --unreleased --strip header

# Generate full CHANGELOG.md file
changelog-full:
    git-cliff --config cliff.toml --output CHANGELOG.md
    @echo "✓ CHANGELOG.md generated successfully!"

# Preview what the next release changelog would look like
changelog-preview:
    @echo "=== Next Release Changelog Preview ==="
    @git-cliff --config cliff.toml --unreleased --strip header

# === Version Management ===

# Show current version
version:
    @cargo metadata --format-version 1 --no-deps | python -c "import json, sys; print(json.load(sys.stdin)['packages'][0]['version'])"

# Bump patch version (0.1.0 -> 0.1.1)
version-patch:
    cargo set-version --bump patch
    @echo "✓ Version bumped to $$(cargo metadata --format-version 1 --no-deps | python -c 'import json, sys; print(json.load(sys.stdin)[\"packages\"][0][\"version\"])')"

# Bump minor version (0.1.0 -> 0.2.0)
version-minor:
    cargo set-version --bump minor
    @echo "✓ Version bumped to $$(cargo metadata --format-version 1 --no-deps | python -c 'import json, sys; print(json.load(sys.stdin)[\"packages\"][0][\"version\"])')"

# Bump major version (0.1.0 -> 1.0.0)
version-major:
    cargo set-version --bump major
    @echo "✓ Version bumped to $$(cargo metadata --format-version 1 --no-deps | python -c 'import json, sys; print(json.load(sys.stdin)[\"packages\"][0][\"version\"])')"

# Preview what the next version would be based on commits
version-preview:
    #!/usr/bin/env bash
    LAST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "none")
    echo "Last tag: $LAST_TAG"
    if [ "$LAST_TAG" = "none" ]; then
        echo "Next version: minor bump (no previous tags)"
    else
        if git log ${LAST_TAG}..HEAD --grep="BREAKING CHANGE" --grep="!:" | grep -q .; then
            echo "Next version: MAJOR bump (breaking changes detected)"
        elif git log ${LAST_TAG}..HEAD --grep="^feat" | grep -q .; then
            echo "Next version: MINOR bump (new features detected)"
        else
            echo "Next version: PATCH bump (bug fixes only)"
        fi
    fi

# === Performance Testing ===

# Run criterion benchmarks (when implemented)
criterion:
    cargo bench --bench '*'

# Generate flamegraph (requires cargo-flamegraph)
flamegraph:
    cargo flamegraph --bench current_state

# === Pre-commit Hooks ===

# Install pre-commit hooks
setup-hooks:
    #!/usr/bin/env bash
    if command -v pwsh &> /dev/null; then
        pwsh -File scripts/setup-hooks.ps1
    else
        bash scripts/setup-hooks.sh
    fi

# Run pre-commit hooks on all files
pre-commit-all:
    pre-commit run --all-files

# Update pre-commit hook versions
pre-commit-update:
    pre-commit autoupdate

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
