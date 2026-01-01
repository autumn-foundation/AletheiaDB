#!/usr/bin/env bash
#
# Lists all git worktrees with their status.
#
# Usage:
#   ./worktree-list.sh

set -euo pipefail

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
GRAY='\033[0;90m'
NC='\033[0m' # No Color

success() { echo -e "${GREEN}$1${NC}"; }
info() { echo -e "${CYAN}$1${NC}"; }
warn() { echo -e "${YELLOW}$1${NC}"; }
err() { echo -e "${RED}$1${NC}"; }

# Get the repo root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -z "$REPO_ROOT" ]; then
    err "Error: Not in a git repository"
    exit 1
fi

info "Git Worktrees:"
echo ""

# Show basic worktree list first
git worktree list

echo ""

# Show detailed status for each worktree
while IFS= read -r line; do
    # Parse worktree path from the output
    WORKTREE_PATH=$(echo "$line" | awk '{print $1}')
    BRANCH=$(echo "$line" | awk '{print $3}' | tr -d '[]')

    if [ -d "$WORKTREE_PATH" ]; then
        # Check for uncommitted changes
        CHANGES=$(cd "$WORKTREE_PATH" && git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
        if [ "$CHANGES" -gt 0 ]; then
            warn "  ! $WORKTREE_PATH: $CHANGES uncommitted change(s)"
        fi

        # Check for unpushed commits (skip if branch doesn't track remote)
        if [ -n "$BRANCH" ] && [ "$BRANCH" != "(bare)" ]; then
            UNPUSHED=$(cd "$WORKTREE_PATH" && git log "origin/$BRANCH..$BRANCH" --oneline 2>/dev/null | wc -l | tr -d ' ' || echo "0")
            if [ "$UNPUSHED" -gt 0 ]; then
                warn "  ^ $WORKTREE_PATH: $UNPUSHED unpushed commit(s)"
            fi
        fi
    fi
done < <(git worktree list)

echo ""
info "Use 'just worktree-new <branch>' to create a new worktree"
info "Use 'just worktree-remove <branch>' to remove a worktree"
