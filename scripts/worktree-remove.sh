#!/usr/bin/env bash
#
# Removes a git worktree and optionally cleans up branches.
#
# Usage:
#   ./worktree-remove.sh feature/my-feature
#   ./worktree-remove.sh fix/bug-name --force

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

# Parse arguments
FORCE=false
BRANCH_NAME=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --force|-f)
            FORCE=true
            shift
            ;;
        *)
            BRANCH_NAME="$1"
            shift
            ;;
    esac
done

if [ -z "$BRANCH_NAME" ]; then
    err "Usage: $0 <branch-name> [--force]"
    err "  Example: $0 feature/add-vector-search"
    exit 1
fi

# Get the repo root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -z "$REPO_ROOT" ]; then
    err "Error: Not in a git repository"
    exit 1
fi

# Sanitize branch name for directory (replace / with -)
DIR_NAME="${BRANCH_NAME//\//-}"
WORKTREE_PATH="$REPO_ROOT/agents/$DIR_NAME"

# Check if worktree exists
if [ ! -d "$WORKTREE_PATH" ]; then
    err "Error: Worktree not found at $WORKTREE_PATH"
    info "Use 'just worktree-list' to see existing worktrees"
    exit 1
fi

info "Removing worktree for branch: $BRANCH_NAME"
info "Location: $WORKTREE_PATH"
echo ""

# Check for uncommitted changes
CHANGES=$(cd "$WORKTREE_PATH" && git status --porcelain 2>/dev/null | wc -l | tr -d ' ')
if [ "$CHANGES" -gt 0 ]; then
    warn "Warning: $CHANGES uncommitted change(s) in worktree!"
    if [ "$FORCE" = false ]; then
        read -p "Continue anyway? (y/N) " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            info "Aborted"
            exit 0
        fi
    fi
fi

# Check for unpushed commits
UNPUSHED=$(cd "$WORKTREE_PATH" && git log "origin/$BRANCH_NAME..$BRANCH_NAME" --oneline 2>/dev/null | wc -l | tr -d ' ' || echo "0")
if [ "$UNPUSHED" -gt 0 ]; then
    warn "Warning: $UNPUSHED unpushed commit(s)!"
    if [ "$FORCE" = false ]; then
        read -p "Continue anyway? (y/N) " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            info "Aborted"
            exit 0
        fi
    fi
fi

# Remove the worktree
info "Removing worktree..."
git worktree remove "$WORKTREE_PATH" --force

success "Worktree removed!"

# Offer to delete local branch
if [ "$FORCE" = false ]; then
    read -p "Delete local branch '$BRANCH_NAME'? (Y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]?$ ]]; then
        git branch -D "$BRANCH_NAME" 2>/dev/null && success "Local branch deleted" || true
    fi
else
    git branch -D "$BRANCH_NAME" 2>/dev/null && success "Local branch deleted" || true
fi

# Offer to delete remote branch
if git ls-remote --heads origin "$BRANCH_NAME" 2>/dev/null | grep -q .; then
    if [ "$FORCE" = false ]; then
        read -p "Delete remote branch 'origin/$BRANCH_NAME'? (y/N) " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            git push origin --delete "$BRANCH_NAME" 2>/dev/null && success "Remote branch deleted" || true
        fi
    fi
fi

echo ""
success "Cleanup complete!"
