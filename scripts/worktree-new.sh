#!/usr/bin/env bash
#
# Creates a new git worktree with a feature or fix branch.
#
# Usage:
#   ./worktree-new.sh feature/my-feature
#   ./worktree-new.sh fix/bug-name

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

# Check argument
if [ $# -lt 1 ]; then
    err "Usage: $0 <branch-name>"
    err "  Example: $0 feature/add-vector-search"
    err "  Example: $0 fix/memory-leak"
    exit 1
fi

BRANCH_NAME="$1"

# Get the repo root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null)
if [ -z "$REPO_ROOT" ]; then
    err "Error: Not in a git repository"
    exit 1
fi

# Validate branch name format
if ! [[ "$BRANCH_NAME" =~ ^(feature|fix)/[a-zA-Z0-9_-]+$ ]]; then
    err "Error: Branch name must be in format 'feature/<name>' or 'fix/<name>'"
    err "  Examples: feature/add-auth, fix/memory-leak"
    exit 1
fi

# Sanitize branch name for directory (replace / with -)
DIR_NAME="${BRANCH_NAME//\//-}"
WORKTREE_PATH="$REPO_ROOT/agents/$DIR_NAME"

# Check if worktree already exists
if [ -d "$WORKTREE_PATH" ]; then
    err "Error: Worktree already exists at $WORKTREE_PATH"
    info "Use 'just worktree-list' to see existing worktrees"
    exit 1
fi

# Check if branch already exists
if git show-ref --verify --quiet "refs/heads/$BRANCH_NAME" 2>/dev/null; then
    err "Error: Branch '$BRANCH_NAME' already exists"
    info "Use a different name or delete the existing branch first"
    exit 1
fi

info "Setting up worktree for branch: $BRANCH_NAME"
info "Location: $WORKTREE_PATH"
echo ""

# Fetch latest from origin
info "Fetching latest from origin..."
git fetch origin trunk

# Create agents directory if it doesn't exist
mkdir -p "$REPO_ROOT/agents"

# Create the worktree with a new branch from trunk
info "Creating worktree and branch..."
git worktree add -b "$BRANCH_NAME" "$WORKTREE_PATH" origin/trunk

# Copy .claude/settings.local.json to the new worktree
SOURCE_SETTINGS="$REPO_ROOT/.claude/settings.local.json"
if [ -f "$SOURCE_SETTINGS" ]; then
    info "Copying Claude settings to worktree..."
    mkdir -p "$WORKTREE_PATH/.claude"
    cp "$SOURCE_SETTINGS" "$WORKTREE_PATH/.claude/"
    success "Claude settings copied!"
fi

echo ""
success "Worktree created successfully!"
echo ""
echo "Next steps:"
echo "  1. cd $WORKTREE_PATH"
echo "  2. Make your changes"
echo "  3. git add . && git commit -m 'Your message'"
echo "  4. just worktree-pr 'PR Title' 'PR Description'"
echo ""
echo "Or run directly:"
echo "  cd $WORKTREE_PATH && code ."
