<#
.SYNOPSIS
    Creates a new git worktree with a feature or fix branch.

.DESCRIPTION
    This script creates a new git worktree for parallel development.
    It fetches the latest trunk, creates a new branch, and sets up
    a worktree in the agents/ directory.

.PARAMETER BranchName
    The name of the branch to create (e.g., feature/my-feature or fix/bug-name)

.EXAMPLE
    .\worktree-new.ps1 feature/add-vector-search
    .\worktree-new.ps1 fix/memory-leak
#>

param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$BranchName
)

$ErrorActionPreference = "Stop"

# Colors for output
function Write-Success { param($msg) Write-Host $msg -ForegroundColor Green }
function Write-Info { param($msg) Write-Host $msg -ForegroundColor Cyan }
function Write-Warn { param($msg) Write-Host $msg -ForegroundColor Yellow }
function Write-Err { param($msg) Write-Host $msg -ForegroundColor Red }

# Get the repo root (where .git is)
$RepoRoot = git rev-parse --show-toplevel 2>$null
if (-not $RepoRoot) {
    Write-Err "Error: Not in a git repository"
    exit 1
}

# Validate branch name format
if (-not ($BranchName -match "^(feature|fix)/[\w\-]+$")) {
    Write-Err "Error: Branch name must be in format 'feature/<name>' or 'fix/<name>'"
    Write-Err "  Examples: feature/add-auth, fix/memory-leak"
    exit 1
}

# Sanitize branch name for directory (replace / with -)
$DirName = $BranchName -replace "/", "-"
$WorktreePath = Join-Path $RepoRoot "agents" $DirName

# Check if worktree already exists
if (Test-Path $WorktreePath) {
    Write-Err "Error: Worktree already exists at $WorktreePath"
    Write-Info "Use 'just worktree-list' to see existing worktrees"
    exit 1
}

# Check if branch already exists
$BranchExists = git show-ref --verify --quiet "refs/heads/$BranchName" 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Err "Error: Branch '$BranchName' already exists"
    Write-Info "Use a different name or delete the existing branch first"
    exit 1
}

Write-Info "Setting up worktree for branch: $BranchName"
Write-Info "Location: $WorktreePath"
Write-Host ""

# Fetch latest from origin
Write-Info "Fetching latest from origin..."
git fetch origin trunk
if ($LASTEXITCODE -ne 0) {
    Write-Err "Error: Failed to fetch from origin"
    exit 1
}

# Create agents directory if it doesn't exist
$AgentsDir = Join-Path $RepoRoot "agents"
if (-not (Test-Path $AgentsDir)) {
    New-Item -ItemType Directory -Path $AgentsDir | Out-Null
}

# Create the worktree with a new branch from trunk
Write-Info "Creating worktree and branch..."
git worktree add -b $BranchName $WorktreePath origin/trunk
if ($LASTEXITCODE -ne 0) {
    Write-Err "Error: Failed to create worktree"
    exit 1
}

# Copy .claude/settings.local.json to the new worktree
$SourceSettings = Join-Path $RepoRoot ".claude" "settings.local.json"
if (Test-Path $SourceSettings) {
    Write-Info "Copying Claude settings to worktree..."
    $TargetClaudeDir = Join-Path $WorktreePath ".claude"
    New-Item -ItemType Directory -Path $TargetClaudeDir -Force | Out-Null
    Copy-Item $SourceSettings -Destination $TargetClaudeDir
    Write-Success "Claude settings copied!"
}

Write-Host ""
Write-Success "Worktree created successfully!"
Write-Host ""
Write-Host "Next steps:"
Write-Host "  1. cd $WorktreePath"
Write-Host "  2. Make your changes"
Write-Host "  3. git add . && git commit -m 'Your message'"
Write-Host "  4. just worktree-pr 'PR Title' 'PR Description'"
Write-Host ""
Write-Host "Or run directly:"
Write-Host "  cd $WorktreePath && code ."
