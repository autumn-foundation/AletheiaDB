# Autonomous Development

GallifreyDB features an **experimental autonomous development workflow** where Claude Code automatically picks up issues, implements solutions, and creates pull requests.

## How It Works

Every day at 3 AM UTC, the autonomous developer:
1. Scans for issues labeled `autonomous-ready`
2. Selects the oldest eligible issue
3. Creates a worktree and implements the fix
4. Runs all quality checks (tests, coverage, lint)
5. Creates a PR with detailed explanation
6. Labels PR as `automated-implementation`

**All automated PRs require human review before merging.**

## The `autonomous-ready` Label

### When to Apply This Label

An issue should be labeled `autonomous-ready` when ALL of the following are true:

#### ✅ Clear Scope
- [ ] The issue has a clear, well-defined problem statement
- [ ] Acceptance criteria are explicit (or obvious)
- [ ] No design decisions or architecture choices needed
- [ ] The solution approach is straightforward

#### ✅ Appropriate Complexity
- [ ] Can be completed in <1 hour of focused work
- [ ] No breaking API changes required
- [ ] No major refactoring needed
- [ ] Doesn't require domain expertise beyond what's in CLAUDE.md

#### ✅ Self-Contained
- [ ] All necessary context is in the issue or linked docs
- [ ] No dependencies on other ongoing work
- [ ] Clear success criteria (tests pass, benchmarks improve, etc.)

#### ✅ Safe to Automate
- [ ] Changes are reversible
- [ ] Low risk if implementation is imperfect
- [ ] Won't block other developers
- [ ] Has clear test coverage requirements

### Good Candidates for `autonomous-ready`

**Documentation:**
- Fix typos or broken links
- Add missing examples
- Clarify confusing explanations
- Update outdated information

**Testing:**
- Add test coverage for uncovered functions
- Add edge case tests
- Fix flaky tests
- Add property-based tests

**Bug Fixes:**
- Small, well-isolated bugs with clear repro steps
- Off-by-one errors
- Missing error handling
- Resource leaks with clear fix

**Code Quality:**
- Remove unused code
- Fix clippy warnings
- Add missing documentation comments
- Simplify overly complex functions

**Performance:**
- Add benchmarks for unbenchmarked code
- Apply obvious optimizations (avoid clones, use iterators)
- Fix inefficient algorithms with clear better alternative

### Bad Candidates (DON'T Label)

**Requires Design Decisions:**
- "Should we use X or Y approach?"
- "How should the API look?"
- "What's the right data structure?"

**High Complexity:**
- Implementing new major features
- Refactoring core architecture
- Performance work requiring profiling
- Algorithm implementations

**Requires Context:**
- Issues that reference tribal knowledge
- "We discussed this in Slack..."
- "As mentioned in the meeting..."

**High Risk:**
- Security-sensitive changes
- Changes to critical hot paths
- Modifications to persistence layer
- Breaking changes

## Success Metrics

Track autonomous development effectiveness:

| Metric | Target | Current |
|--------|--------|---------|
| PR Success Rate | >70% | TBD |
| First-Time CI Pass | >80% | TBD |
| Merged Without Changes | >50% | TBD |
| Average Review Time | <24h | TBD |
| Issues Skipped (Self-Assessment) | <30% | TBD |

## Safety Mechanisms

### Rate Limiting
- **Max 1 attempt per day** (3 AM UTC)
- **Max 5 open automated PRs** at once
- **7-day cooldown** on failed issues

### Quality Gates
All automated PRs must:
- ✅ Pass all CI checks (tests, coverage, lint, security, benchmarks)
- ✅ Meet coverage thresholds (≥85%)
- ✅ Follow conventional commit format
- ✅ Include test coverage for new code

### Human Oversight
- **All automated PRs require human approval**
- PRs labeled `automated-implementation` for easy filtering
- Autonomous developer comments on issues with reasoning
- Failed attempts logged for human review

### Failure Handling
If the autonomous developer:
- **Can't understand the issue** → Comments explaining why, skips issue
- **Encounters test failures** → Creates draft PR with explanation
- **Gets stuck** → Comments on issue after 30 minutes, exits gracefully
- **Workflow times out** → Fails safely, comments on issue

## Manual Triggering

You can manually trigger the workflow for testing:

```bash
# Via GitHub UI:
# Actions → Autonomous Development → Run workflow

# Via GitHub CLI:
gh workflow run autonomous-dev.yml

# Target specific issue:
gh workflow run autonomous-dev.yml -f issue_number=123
```

## Monitoring

### View Automated PRs
```bash
gh pr list --label "automated-implementation"
```

### Success Rate
```bash
# Merged automated PRs
gh pr list --label "automated-implementation" --state merged --limit 100

# Total automated PRs
gh pr list --label "automated-implementation" --state all --limit 100
```

### Recent Activity
```bash
gh run list --workflow=autonomous-dev.yml --limit 10
```

## Best Practices for Issue Authors

To make your issue eligible for autonomous development:

1. **Write clear acceptance criteria:**
   ```markdown
   ## Acceptance Criteria
   - [ ] Function `foo()` handles empty input
   - [ ] Test coverage for edge case added
   - [ ] Documentation updated
   ```

2. **Link to relevant code:**
   ```markdown
   The bug is in `src/storage/wal.rs:142`
   ```

3. **Provide examples:**
   ```markdown
   Current behavior: `get_node(0)` panics
   Expected behavior: `get_node(0)` returns `Err(Error::InvalidId)`
   ```

4. **Keep scope small:**
   - One issue = one focused change
   - Split large issues into smaller autonomous-ready pieces

5. **Include test guidance:**
   ```markdown
   Add a test in `tests/storage/wal_tests.rs` that verifies...
   ```

## Troubleshooting

### Issue wasn't picked up
- Verify `autonomous-ready` label is applied
- Check if 5 automated PRs are already open
- Check if issue was attempted in last 7 days
- Review workflow logs for errors

### Automated PR has issues
- Review PR comments for autonomous developer's reasoning
- Provide feedback directly on PR
- Consider removing `autonomous-ready` label if issue needs human judgment

### Too many automated PRs
The workflow self-limits to 5 open PRs. Merge or close existing automated PRs to unblock.

## Future Enhancements

### Phase 2: Learning from History
Use **GallifreyDB itself** to track:
- Which patterns succeeded/failed
- Common pitfalls by issue type
- Codebase evolution context
- Historical fixes for similar issues

### Phase 3: Confidence Scoring
```rust
struct IssueConfidence {
    scope_clarity: f64,      // How clear is the issue?
    complexity_estimate: f64, // How hard is it?
    historical_success: f64,  // Similar issues succeeded?
    test_coverage: f64,       // Good test hooks exist?
}
```

### Phase 4: Multi-Issue Planning
- Autonomous developer plans multi-day sprints
- Prioritizes issues by impact and confidence
- Coordinates with human developers

## Philosophical Notes

This is **genuinely experimental** - we're pushing the boundaries of AI-assisted development. The goal isn't to replace human developers, but to:

1. **Unlock velocity** - Free humans for creative/strategic work
2. **Maintain momentum** - Progress even when team is busy
3. **Demonstrate AI capabilities** - Show what's possible in 2026
4. **Dogfood GallifreyDB** - Use temporal reasoning for software development

**The vision:** A persistent AI that learns the codebase over time, understands patterns, and makes increasingly sophisticated contributions.

This is infrastructure for **autonomous agents with memory**. 🚀

## Questions?

- **Is this safe?** Yes - all PRs require human review, quality gates are strict
- **Will it waste CI resources?** Rate limited to 1/day, max 5 open PRs
- **What if it breaks something?** Can't merge without approval, all changes are reversible
- **Can I disable it?** Yes - just don't add `autonomous-ready` labels
- **Why 3 AM UTC?** Low traffic time, doesn't interfere with human development

## References

- ADR-0015: CI/CD Automation Workflow
- CLAUDE.md: Development guidelines
- WORKTREE_WORKFLOW.md: Parallel development
- `.github/workflows/autonomous-dev.yml`: Workflow implementation
