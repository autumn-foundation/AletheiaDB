<#
.SYNOPSIS
    Removes a git worktree and optionally cleans up branches.

.DESCRIPTION
    Removes a worktree from the agents/ directory and offers to delete
    the local and remote branches. Warns about uncommitted changes.

.PARAMETER BranchName
    The name of the branch whose worktree to remove (e.g., feature/my-feature)

.PARAMETER Force
    Skip confirmation prompts and force removal

.EXAMPLE
    .\worktree-remove.ps1 feature/add-vector-search
    .\worktree-remove.ps1 fix/memory-leak -Force
#>

param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$BranchName,

    [switch]$Force
)

$ErrorActionPreference = "Stop"

# Colors for output
function Write-Success { param($msg) Write-Host $msg -ForegroundColor Green }
function Write-Info { param($msg) Write-Host $msg -ForegroundColor Cyan }
function Write-Warn { param($msg) Write-Host $msg -ForegroundColor Yellow }
function Write-Err { param($msg) Write-Host $msg -ForegroundColor Red }

# Get the repo root
$RepoRoot = git rev-parse --show-toplevel 2>$null
if (-not $RepoRoot) {
    Write-Err "Error: Not in a git repository"
    exit 1
}

# Sanitize branch name for directory (replace / with -)
$DirName = $BranchName -replace "/", "-"
$WorktreePath = Join-Path $RepoRoot "agents" $DirName

# Check if worktree exists
if (-not (Test-Path $WorktreePath)) {
    Write-Err "Error: Worktree not found at $WorktreePath"
    Write-Info "Use 'just worktree-list' to see existing worktrees"
    exit 1
}

Write-Info "Removing worktree for branch: $BranchName"
Write-Info "Location: $WorktreePath"
Write-Host ""

# Check for uncommitted changes
Push-Location $WorktreePath
try {
    $Status = git status --porcelain 2>$null
    if ($Status) {
        $Count = ($Status -split "`n").Count
        Write-Warn "Warning: $Count uncommitted change(s) in worktree!"
        if (-not $Force) {
            $Response = Read-Host "Continue anyway? (y/N)"
            if ($Response -ne "y" -and $Response -ne "Y") {
                Write-Info "Aborted"
                exit 0
            }
        }
    }

    # Check for unpushed commits
    $Unpushed = git log "origin/$BranchName..$BranchName" --oneline 2>$null
    if ($LASTEXITCODE -eq 0 -and $Unpushed) {
        $Count = ($Unpushed -split "`n").Count
        Write-Warn "Warning: $Count unpushed commit(s)!"
        if (-not $Force) {
            $Response = Read-Host "Continue anyway? (y/N)"
            if ($Response -ne "y" -and $Response -ne "Y") {
                Write-Info "Aborted"
                exit 0
            }
        }
    }
}
finally {
    Pop-Location
}

# Remove the worktree
Write-Info "Removing worktree..."
git worktree remove $WorktreePath --force
if ($LASTEXITCODE -ne 0) {
    Write-Err "Error: Failed to remove worktree"
    exit 1
}

Write-Success "Worktree removed!"

# Offer to delete local branch
if (-not $Force) {
    $Response = Read-Host "Delete local branch '$BranchName'? (Y/n)"
    if ($Response -eq "" -or $Response -eq "y" -or $Response -eq "Y") {
        git branch -D $BranchName 2>$null
        if ($LASTEXITCODE -eq 0) {
            Write-Success "Local branch deleted"
        }
    }
}
else {
    git branch -D $BranchName 2>$null
    if ($LASTEXITCODE -eq 0) {
        Write-Success "Local branch deleted"
    }
}

# Offer to delete remote branch
$RemoteExists = git ls-remote --heads origin $BranchName 2>$null
if ($RemoteExists) {
    if (-not $Force) {
        $Response = Read-Host "Delete remote branch 'origin/$BranchName'? (y/N)"
        if ($Response -eq "y" -or $Response -eq "Y") {
            git push origin --delete $BranchName 2>$null
            if ($LASTEXITCODE -eq 0) {
                Write-Success "Remote branch deleted"
            }
        }
    }
}

Write-Host ""
Write-Success "Cleanup complete!"
