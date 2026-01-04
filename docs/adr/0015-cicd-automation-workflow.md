# ADR-0015: CI/CD Automation and Development Workflow

**Status:** Accepted
**Date:** 2026-01-03
**Deciders:** madmax983, CI Claude
**Categories:** CI/CD, Development Workflow, Automation, AI-Assisted Development

## Context

GallifreyDB is developed with multiple parallel Claude instances working simultaneously via git worktrees, creating unique challenges for maintaining code quality, dependency management, and release processes. As the project matures toward a public release, we need robust automation to:

1. **Manage dependencies safely** - Regular updates without breaking changes
2. **Enforce code quality** - Consistent standards across all contributions
3. **Streamline releases** - Reduce manual overhead for version bumps and changelogs
4. **Enable AI velocity** - Support multiple Claude instances working in parallel
5. **Maintain security** - Proactive vulnerability detection and patching

The manual overhead of reviewing Dependabot PRs, generating changelogs, and enforcing code quality was becoming a bottleneck to development velocity.

## Decision

We will implement a comprehensive CI/CD automation suite with the following components:

### 1. Dependabot with Intelligent Grouping

**Configuration:** `.github/dependabot.yml`

- **Daily Cargo updates** (2 AM UTC) to catch security patches quickly
- **Weekly GitHub Actions updates** to reduce noise
- **Grouped dependencies** by purpose:
  - `dev-dependencies`: testing/benchmarking tools (patch/minor updates)
  - `core-dependencies`: critical runtime deps (patch-only for stability)
  - `optional-dependencies`: features like Tracy profiling (all updates)
- **Core stability**: usearch, dashmap, crc32fast limited to patch updates

### 2. Auto-Merge for Safe Updates

**Workflow:** `.github/workflows/auto-merge.yml`

- Automatically approve and merge **patch/minor** Dependabot updates
- Block **major** version updates for manual review
- Wait for all CI checks (tests, coverage, lint, security, benchmarks)
- **Safety features:**
  - 30-minute timeout to prevent indefinite waiting
  - Only runs on `dependabot[bot]` PRs with `auto-merge-candidate` label
  - Comments on failures with actionable information

### 3. Automated Changelog Generation

**Tool:** git-cliff
**Configuration:** `cliff.toml`

- Conventional commit parsing (feat, fix, perf, docs, etc.)
- Automatic categorization and emoji decoration
- Includes PR numbers and links
- **Timing:** Generated in version-bump PR (not during release) to prevent race conditions

### 4. Version Bump Workflow

**Workflow:** `.github/workflows/version-bump.yml`

- Manual trigger with auto/patch/minor/major options
- **Auto-detection** based on conventional commits:
  - `BREAKING CHANGE` or `!:` → major
  - `feat:` → minor
  - `fix:`, `perf:`, etc. → patch
- Creates PR with:
  - Updated Cargo.toml version
  - Generated CHANGELOG.md
  - Auto-merge-candidate label
- **Security:** Branch-restricted to `trunk` only

### 5. Pre-commit Hooks

**Configuration:** `.pre-commit-config.yaml`
**Setup Scripts:** `scripts/setup-hooks.{sh,ps1}`

**Stages:**
- **pre-commit**: formatting (rustfmt), linting (clippy), compilation (cargo check)
- **commit-msg**: conventional commit enforcement (commitizen)
- **pre-push**: tests, security audit (cargo audit)

**Philosophy:**
- Local clippy runs **without** `-D warnings` to allow iteration
- CI enforces `-D warnings` for merge gates
- Cross-platform setup scripts (Bash + PowerShell)

### 6. Weekly AI Code Health Scans

**Workflow:** `.github/workflows/code-health-scan.yml`

- **Schedule:** Every Monday at 9 AM UTC
- **Three specialized scans:**
  1. **Security**: Vulnerabilities, unsafe code, auth/crypto issues
  2. **Performance**: Bottlenecks, allocations, algorithmic complexity
  3. **Code Quality**: Architecture, maintainability, tech debt
- Uses `anthropics/claude-code-action@v1`
- **Safeguards:**
  - Checks for existing issues to prevent duplicates (max 5 per category)
  - Auto-labels with `automated-scan` + category
  - Creates actionable GitHub issues

### 7. Security Policy

**Document:** `SECURITY.md`

- Responsible disclosure process
- Supported versions (0.x = best-effort, 1.0+ = SemVer guarantees)
- Security best practices
- Known pre-1.0 limitations

### 8. Autonomous Development (Experimental)

**Workflow:** `.github/workflows/autonomous-dev.yml`
**Documentation:** `docs/AUTONOMOUS_DEVELOPMENT.md`

- **Daily autonomous issue resolution** (3 AM UTC)
- Claude Code picks up issues labeled `autonomous-ready`
- Creates worktree, implements solution, runs tests, creates PR
- **Experimental feature** pushing boundaries of AI-assisted development

**Safety mechanisms:**
- Only works on `autonomous-ready` labeled issues (human-curated)
- Max 1 PR per day, max 5 open automated PRs
- All PRs require human review (no auto-merge)
- Must pass all CI quality gates
- Self-assessment: autonomous developer evaluates if it can complete the issue
- Failure handling: comments on issue if unable to complete

**Issue selection criteria:**
- Clear scope and acceptance criteria
- Appropriate complexity (docs, tests, small bugs)
- Self-contained with all context provided
- Avoids: architecture changes, breaking changes, design decisions

**The vision:** Use GallifreyDB itself to track autonomous development patterns, success rates, and codebase context over time - demonstrating **temporal reasoning for AI software development**.

## Consequences

### Positive

- **Reduced manual overhead**: Dependabot PRs auto-merge when safe, saving hours per week
- **Faster security patching**: Daily checks mean vulnerabilities are caught within 24 hours
- **Consistent code quality**: Pre-commit hooks prevent style drift
- **Release confidence**: Automated changelog generation ensures no changes are forgotten
- **AI velocity unlocked**: Claude instances can work in parallel with automated quality gates
- **Proactive issue detection**: Weekly scans identify tech debt before it becomes critical
- **Better documentation**: Conventional commits enforce clear intent
- **GitHub Pages integration**: Documentation and benchmarks auto-publish
- **Continuous forward momentum**: Autonomous developer works on issues even when humans are busy
- **Cutting-edge AI demonstration**: Showcases state-of-the-art autonomous AI development
- **Dogfooding opportunity**: Can use GallifreyDB to track autonomous development patterns

### Negative

- **Setup complexity**: New contributors need to install pre-commit + cargo-audit
- **CI time overhead**: Auto-merge waits for all checks (can take 10-30 minutes)
- **False positives**: AI scans may create spurious issues requiring triage
- **Maintenance burden**: Workflows themselves need maintenance as GitHub Actions evolve
- **Cognitive load**: Contributors must learn conventional commit format
- **Autonomous PR quality variance**: Automated PRs may need more review/iteration than human PRs
- **API cost**: Daily autonomous development consumes Claude API credits
- **Experimental risk**: Autonomous development is bleeding-edge, may have unexpected failures

### Neutral

- **Opinionated workflow**: Strong opinions about commit messages, versioning strategy
- **GitHub-specific**: Heavy reliance on GitHub Actions (vendor lock-in)
- **Weekly scan frequency**: Trade-off between noise and freshness (could adjust)

## Alternatives Considered

### Alternative 1: Manual Dependabot Review

Keep status quo of manually reviewing every Dependabot PR.

**Rejected because:**
- Does not scale with multiple Claude instances creating PRs
- Security patches delayed by manual review latency
- Cognitive overhead for trivial patch updates
- Blocks development velocity

### Alternative 2: Daily AI Code Scans

Run AI code health scans every day instead of weekly.

**Rejected because:**
- Excessive GitHub Actions minutes consumption
- Too much noise for active development phase
- Weekly cadence sufficient for catching issues
- Can always run on-demand via `workflow_dispatch`

### Alternative 3: Squash Pre-commit Stages

Run all hooks (including tests) on every commit.

**Rejected because:**
- Tests take ~30 seconds, slows down tight iteration loops
- Audit checks are slow and change infrequently
- pre-push stage provides good balance
- Developers can run `just pre-commit-all` manually if desired

### Alternative 4: Changelog in Release Workflow

Generate CHANGELOG.md during the release process instead of version-bump PR.

**Rejected because:**
- Race condition: pushing to trunk during release can fail
- Harder to review changelog before release
- Separating version bump PR from release tag is cleaner
- Follows "review before merge" principle

### Alternative 5: Renovate instead of Dependabot

Use Renovate Bot for dependency management.

**Rejected because:**
- Dependabot is native to GitHub (no third-party setup)
- Simpler configuration for our use case
- Sufficient grouping and scheduling capabilities
- Native integration with GitHub Security Advisories

## Implementation Notes

### Worktree Compatibility

All workflows are worktree-aware and work correctly when multiple Claude instances have active worktrees. The auto-merge workflow only triggers on the main repo PR, not worktree branches.

### Conventional Commit Format

Developers should use:
- `feat:` for new features
- `fix:` for bug fixes
- `perf:` for performance improvements
- `docs:` for documentation
- `chore:` for maintenance tasks
- `!` suffix or `BREAKING CHANGE:` footer for breaking changes

Examples:
```
feat: Add vector similarity search API
fix: Prevent race condition in WAL recovery
perf!: Remove lock from hot path (BREAKING: API changes)
```

### Pre-commit Setup

New contributors run:
```bash
# Linux/Mac
./scripts/setup-hooks.sh

# Windows
.\scripts\setup-hooks.ps1
```

### Triggering Version Bump

Maintainers trigger via GitHub Actions UI:
1. Navigate to Actions → Version Bump
2. Click "Run workflow"
3. Select bump type (auto recommended)
4. Review PR, approve, merge
5. Tag and push: `git tag v0.x.y && git push origin v0.x.y`
6. Release workflow automatically publishes

### CI Workflow Interaction

The automation suite integrates with existing CI workflows:
- `ci.yml`: Test Suite - required for auto-merge
- `coverage.yml`: Code Coverage - required for auto-merge
- `lint.yml`: Clippy + fmt - required for auto-merge
- `security.yml`: cargo-audit - required for auto-merge
- `benchmarks.yml`: Performance regression - required for auto-merge

### GitHub Permissions Required

Repository settings → Actions → General:
- Workflow permissions: **Read and write permissions**
- Allow GitHub Actions to create/approve pull requests: **Enabled**

Repository settings → Pages:
- Source: **gh-pages branch**

Repository settings → Secrets and variables → Actions:
- **ANTHROPIC_API_KEY**: Required for autonomous development and code health scans

## References

- PR #160: Implementation of CI/CD automation suite
- [Conventional Commits](https://www.conventionalcommits.org/)
- [git-cliff](https://git-cliff.org/) - Changelog generator
- [pre-commit](https://pre-commit.com/) - Git hook framework
- [Dependabot](https://docs.github.com/en/code-security/dependabot)
- [anthropics/claude-code-action](https://github.com/anthropics/claude-code-action)
- CLAUDE.md: "Development Workflow" section
- WORKTREE_WORKFLOW.md: Parallel development with git worktrees
- docs/AUTONOMOUS_DEVELOPMENT.md: Autonomous development documentation
- .github/ISSUE_TEMPLATE/autonomous-ready.md: Issue template for autonomous tasks
