# Git Worktree Workflow for Parallel Development

This document describes how to use git worktrees for parallel development, enabling multiple Claude instances (or developers) to work simultaneously on different features.

## For Claude Instances

**IMPORTANT**: When you start working on ANY implementation task:

1. **Check if you're in a worktree**:
   ```bash
   git worktree list
   ```
   If you see only one entry (the main repo), you need to create a worktree.

2. **Create a worktree for your task**:
   ```bash
   just worktree-new feature/task-name   # or fix/task-name
   cd agents/feature-task-name
   ```

3. **Work in the worktree**, then commit and create PR:
   ```bash
   # After making changes
   git add . && git commit -m "feat: description"
   just worktree-pr "PR Title" "Description"
   ```

The worktree automatically includes your Claude settings (`.claude/settings.local.json`).

## Quick Start

```bash
# Create a new worktree for your feature
just worktree-new feature/my-feature

# Navigate to the worktree
cd agents/feature-my-feature

# Make your changes, then commit
git add .
git commit -m "feat: add my feature"

# Create a PR to trunk
just worktree-pr "Add my feature" "Description of changes"

# After PR is merged, clean up
just worktree-remove feature/my-feature
```

## Commands Reference

| Command | Description |
|---------|-------------|
| `just worktree-new <branch>` | Create new worktree with feature/fix branch |
| `just worktree-list` | List all worktrees with status |
| `just worktree-remove <branch>` | Remove worktree and clean up branches |
| `just worktree-pr "<title>" "<body>"` | Push and create PR to trunk |

## Branch Naming Convention

Use the following prefixes:

- `feature/` - New features (e.g., `feature/vector-search`, `feature/add-auth`)
- `fix/` - Bug fixes (e.g., `fix/memory-leak`, `fix/query-timeout`)

Examples:
```bash
just worktree-new feature/temporal-indexes
just worktree-new fix/version-chain-ordering
```

## Directory Structure

Worktrees are created in the `agents/` directory:

```
gallifreydb/
├── agents/
│   ├── feature-my-feature/     # Worktree for feature/my-feature
│   ├── fix-memory-leak/        # Worktree for fix/memory-leak
│   └── ...
├── src/
├── ...
```

The `agents/` directory is gitignored, so worktrees won't be committed.

## Complete Workflow Example

### 1. Start a new feature

```bash
# From the main repository
just worktree-new feature/add-compression

# Output:
# Setting up worktree for branch: feature/add-compression
# Location: /path/to/gallifreydb/agents/feature-add-compression
# Fetching latest from origin...
# Creating worktree and branch...
# Worktree created successfully!
```

### 2. Work on the feature

```bash
cd agents/feature-add-compression

# Open in your editor
code .

# Make changes...
# Run tests
just test

# Commit your work
git add .
git commit -m "feat: implement compression for historical storage"
```

### 3. Create a Pull Request

```bash
# From within the worktree
just worktree-pr "Implement compression for historical storage" "Adds anchor+delta compression reducing storage by 5-6X"

# This will:
# 1. Push the branch to origin
# 2. Create a PR targeting trunk
# 3. Open the PR in your browser
```

### 4. Clean up after merge

After your PR is merged:

```bash
# From the main repository (or any worktree)
just worktree-remove feature/add-compression

# This will:
# 1. Remove the worktree directory
# 2. Optionally delete the local branch
# 3. Optionally delete the remote branch
```

## Working with Multiple Features

You can have multiple worktrees active simultaneously:

```bash
# Terminal 1 - Claude instance working on feature A
just worktree-new feature/vector-search
cd agents/feature-vector-search
# ... work on vector search

# Terminal 2 - Claude instance working on feature B
just worktree-new feature/query-optimizer
cd agents/feature-query-optimizer
# ... work on query optimizer

# Terminal 3 - Claude instance fixing a bug
just worktree-new fix/memory-leak
cd agents/fix-memory-leak
# ... fix the bug
```

All three can work independently without conflicts.

## Best Practices

1. **Always create from fresh trunk**: The `worktree-new` script fetches the latest trunk before creating your branch.

2. **Keep worktrees focused**: Each worktree should focus on a single feature or fix.

3. **Clean up promptly**: Remove worktrees after PRs are merged to avoid clutter.

4. **Don't modify the main worktree**: Use `agents/` worktrees for all development work.

5. **Commit frequently**: Make small, focused commits within your worktree.

## Troubleshooting

### Worktree already exists

```
Error: Worktree already exists at /path/to/agents/feature-name
```

Either use a different branch name, or remove the existing worktree:
```bash
just worktree-remove feature/name
```

### Branch already exists

```
Error: Branch 'feature/name' already exists
```

The branch might exist from a previous attempt. Delete it:
```bash
git branch -D feature/name
```

### Uncommitted changes warning

When removing a worktree with uncommitted changes:
```
Warning: 5 uncommitted change(s) in worktree!
Continue anyway? (y/N)
```

Either commit your changes first, or confirm to discard them.

### Cannot push to origin

If `gh` CLI isn't authenticated:
```bash
gh auth login
```

## Scripts Location

The worktree scripts are located in `scripts/`:

- `scripts/worktree-new.ps1` / `scripts/worktree-new.sh`
- `scripts/worktree-list.ps1` / `scripts/worktree-list.sh`
- `scripts/worktree-remove.ps1` / `scripts/worktree-remove.sh`
- `scripts/worktree-pr.ps1` / `scripts/worktree-pr.sh`

The `just` commands automatically select the appropriate script based on your environment.
