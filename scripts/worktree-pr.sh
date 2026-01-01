#!/usr/bin/env bash
#
# Pushes the current branch and creates a PR to trunk.
#
# Usage:
#   ./worktree-pr.sh "PR Title"
#   ./worktree-pr.sh "PR Title" "PR Description"

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

success() { echo -e "${GREEN}$1${NC}"; }
info() { echo -e "${CYAN}$1${NC}"; }
warn() { echo -e "${YELLOW}$1${NC}"; }
err() { echo -e "${RED}$1${NC}"; }

# Check arguments
if [ $# -lt 1 ]; then
    err "Usage: $0 <title> [body]"
    err "  Example: $0 'Add vector search feature'"
    err "  Example: $0 'Fix memory leak' 'Fixes issue #123'"
    exit 1
fi

TITLE="$1"
BODY="${2:-}"

# Check if gh is installed
if ! command -v gh &> /dev/null; then
    err "Error: GitHub CLI (gh) is not installed"
    info "Install it from: https://cli.github.com/"
    exit 1
fi

# Get current branch
CURRENT_BRANCH=$(git branch --show-current 2>/dev/null)
if [ -z "$CURRENT_BRANCH" ]; then
    err "Error: Could not determine current branch"
    exit 1
fi

# Don't allow PRs from trunk or main
if [ "$CURRENT_BRANCH" = "trunk" ] || [ "$CURRENT_BRANCH" = "main" ] || [ "$CURRENT_BRANCH" = "master" ]; then
    err "Error: Cannot create PR from '$CURRENT_BRANCH'"
    info "Create a feature branch first with 'just worktree-new feature/name'"
    exit 1
fi

info "Creating PR for branch: $CURRENT_BRANCH"
echo ""

# Check for uncommitted changes
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
    err "Error: You have uncommitted changes"
    info "Commit your changes first:"
    echo "  git add . && git commit -m 'Your message'"
    exit 1
fi

# Push to origin
info "Pushing to origin..."
git push -u origin "$CURRENT_BRANCH"

success "Pushed to origin/$CURRENT_BRANCH"
echo ""

# Create PR
info "Creating pull request..."

# Build the PR body
if [ -n "$BODY" ]; then
    FULL_BODY=$(cat <<EOF
## Summary
$BODY

---
Created from worktree workflow.
EOF
)
else
    FULL_BODY="Created from worktree workflow."
fi

# Create the PR
if ! PR_OUTPUT=$(gh pr create --base trunk --title "$TITLE" --body "$FULL_BODY" 2>&1); then
    # Check if PR already exists
    if echo "$PR_OUTPUT" | grep -q "already exists"; then
        warn "A pull request already exists for this branch"
        gh pr view --web
        exit 0
    fi
    err "Error: Failed to create PR"
    err "$PR_OUTPUT"
    exit 1
fi

echo ""
success "Pull request created!"
echo "$PR_OUTPUT"

# Open in browser
echo ""
info "Opening PR in browser..."
gh pr view --web
