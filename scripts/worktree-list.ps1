<#
.SYNOPSIS
    Lists all git worktrees with their status.

.DESCRIPTION
    Shows all active worktrees, their branches, and status information
    including uncommitted changes and unpushed commits.

.EXAMPLE
    .\worktree-list.ps1
#>

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

Write-Info "Git Worktrees:"
Write-Host ""

# Get worktree list
$Worktrees = git worktree list --porcelain | Out-String

# Parse and display each worktree
$CurrentWorktree = $null
$CurrentBranch = $null

foreach ($line in (git worktree list --porcelain) -split "`n") {
    if ($line -match "^worktree (.+)$") {
        if ($CurrentWorktree) {
            # Output previous worktree info
            Write-WorktreeInfo $CurrentWorktree $CurrentBranch
        }
        $CurrentWorktree = $Matches[1]
        $CurrentBranch = $null
    }
    elseif ($line -match "^branch refs/heads/(.+)$") {
        $CurrentBranch = $Matches[1]
    }
}

# Output last worktree
if ($CurrentWorktree) {
    Write-WorktreeInfo $CurrentWorktree $CurrentBranch
}

function Write-WorktreeInfo {
    param($Path, $Branch)

    $IsMain = $Path -eq $RepoRoot
    $Prefix = if ($IsMain) { "[MAIN]" } else { "      " }

    Write-Host "$Prefix $Branch" -NoNewline
    Write-Host " -> " -NoNewline -ForegroundColor DarkGray
    Write-Host $Path -ForegroundColor DarkGray

    # Check for uncommitted changes
    Push-Location $Path
    try {
        $Status = git status --porcelain 2>$null
        if ($Status) {
            $Count = ($Status -split "`n").Count
            Write-Warn "        ! $Count uncommitted change(s)"
        }

        # Check for unpushed commits (only for non-main worktrees)
        if (-not $IsMain -and $Branch) {
            $Unpushed = git log "origin/$Branch..$Branch" --oneline 2>$null
            if ($LASTEXITCODE -eq 0 -and $Unpushed) {
                $Count = ($Unpushed -split "`n").Count
                Write-Warn "        ^ $Count unpushed commit(s)"
            }
        }
    }
    finally {
        Pop-Location
    }

    Write-Host ""
}

# Simple list view (git worktree list already gives good output)
git worktree list

Write-Host ""
Write-Info "Use 'just worktree-new <branch>' to create a new worktree"
Write-Info "Use 'just worktree-remove <branch>' to remove a worktree"
