<#
.SYNOPSIS
    Pushes the current branch and creates a PR to trunk.

.DESCRIPTION
    Ensures all changes are committed, pushes to origin, and creates
    a pull request to trunk using the GitHub CLI (gh).

.PARAMETER Title
    The title for the pull request

.PARAMETER Body
    The body/description for the pull request (optional)

.EXAMPLE
    .\worktree-pr.ps1 "Add vector search feature"
    .\worktree-pr.ps1 "Fix memory leak" "Fixes issue #123"
#>

param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$Title,

    [Parameter(Position=1)]
    [string]$Body = ""
)

$ErrorActionPreference = "Stop"

# Colors for output
function Write-Success { param($msg) Write-Host $msg -ForegroundColor Green }
function Write-Info { param($msg) Write-Host $msg -ForegroundColor Cyan }
function Write-Warn { param($msg) Write-Host $msg -ForegroundColor Yellow }
function Write-Err { param($msg) Write-Host $msg -ForegroundColor Red }

# Check if gh is installed
$GhExists = Get-Command gh -ErrorAction SilentlyContinue
if (-not $GhExists) {
    Write-Err "Error: GitHub CLI (gh) is not installed"
    Write-Info "Install it from: https://cli.github.com/"
    exit 1
}

# Get current branch
$CurrentBranch = git branch --show-current 2>$null
if (-not $CurrentBranch) {
    Write-Err "Error: Could not determine current branch"
    exit 1
}

# Don't allow PRs from trunk or main
if ($CurrentBranch -eq "trunk" -or $CurrentBranch -eq "main" -or $CurrentBranch -eq "master") {
    Write-Err "Error: Cannot create PR from '$CurrentBranch'"
    Write-Info "Create a feature branch first with 'just worktree-new feature/name'"
    exit 1
}

Write-Info "Creating PR for branch: $CurrentBranch"
Write-Host ""

# Check for uncommitted changes
$Status = git status --porcelain 2>$null
if ($Status) {
    Write-Err "Error: You have uncommitted changes"
    Write-Info "Commit your changes first:"
    Write-Host "  git add . && git commit -m 'Your message'"
    exit 1
}

# Push to origin
Write-Info "Pushing to origin..."
git push -u origin $CurrentBranch
if ($LASTEXITCODE -ne 0) {
    Write-Err "Error: Failed to push to origin"
    exit 1
}

Write-Success "Pushed to origin/$CurrentBranch"
Write-Host ""

# Create PR
Write-Info "Creating pull request..."

# Build the PR body
$FullBody = @"
## Summary
$Body

---
Created from worktree workflow.
"@

if ([string]::IsNullOrWhiteSpace($Body)) {
    $FullBody = "Created from worktree workflow."
}

# Create the PR
$PrOutput = gh pr create --base trunk --title $Title --body $FullBody 2>&1
if ($LASTEXITCODE -ne 0) {
    # Check if PR already exists
    if ($PrOutput -match "already exists") {
        Write-Warn "A pull request already exists for this branch"
        gh pr view --web
        exit 0
    }
    Write-Err "Error: Failed to create PR"
    Write-Err $PrOutput
    exit 1
}

Write-Host ""
Write-Success "Pull request created!"
Write-Host $PrOutput

# Open in browser
Write-Host ""
Write-Info "Opening PR in browser..."
gh pr view --web
