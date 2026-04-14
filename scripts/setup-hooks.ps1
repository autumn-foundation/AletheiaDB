# Setup pre-commit hooks for AletheiaDB (PowerShell)
$ErrorActionPreference = "Stop"

Write-Host "🔧 Setting up pre-commit hooks for AletheiaDB..." -ForegroundColor Cyan

# Check if pre-commit is installed
if (-not (Get-Command pre-commit -ErrorAction SilentlyContinue)) {
    Write-Host "📦 Installing pre-commit..." -ForegroundColor Yellow
    if (Get-Command pip3 -ErrorAction SilentlyContinue) {
        pip3 install pre-commit
    }
    elseif (Get-Command pip -ErrorAction SilentlyContinue) {
        pip install pre-commit
    }
    else {
        Write-Host "❌ Error: pip not found. Please install Python and pip first." -ForegroundColor Red
        Write-Host "Visit: https://pip.pypa.io/en/stable/installation/" -ForegroundColor Yellow
        exit 1
    }
}

# Check if cargo-audit is installed
if (-not (Get-Command cargo-audit -ErrorAction SilentlyContinue)) {
    Write-Host "📦 Installing cargo-audit..." -ForegroundColor Yellow
    cargo install cargo-audit
}

# Check if cargo-edit is installed (for version bumping)
if (-not (Get-Command cargo-set-version -ErrorAction SilentlyContinue)) {
    Write-Host "📦 Installing cargo-edit..." -ForegroundColor Yellow
    cargo install cargo-edit
}

# Install pre-commit hooks
Write-Host "⚙️  Installing pre-commit hooks..." -ForegroundColor Cyan
pre-commit install --hook-type pre-commit --hook-type commit-msg --hook-type pre-push

# Run hooks on all files to verify setup
Write-Host "✅ Running pre-commit on all files to verify setup..." -ForegroundColor Cyan
try {
    pre-commit run --all-files
}
catch {
    Write-Host ""
    Write-Host "⚠️  Some hooks failed, but that's okay for initial setup!" -ForegroundColor Yellow
    Write-Host "   Run 'cargo fmt' and 'cargo clippy --fix' to fix issues." -ForegroundColor Yellow
}

Write-Host ""
Write-Host "✅ Pre-commit hooks successfully installed!" -ForegroundColor Green
Write-Host ""
Write-Host "📝 Hooks installed:" -ForegroundColor Cyan
Write-Host "   - pre-commit: Runs on 'git commit' (format, clippy, checks)"
Write-Host "   - commit-msg: Enforces conventional commits"
Write-Host "   - pre-push: Runs tests and security audit before push"
Write-Host ""
Write-Host "💡 Tips:" -ForegroundColor Cyan
Write-Host "   - Run hooks manually: pre-commit run --all-files"
Write-Host "   - Update hooks: pre-commit autoupdate"
Write-Host "   - Skip hooks: git commit --no-verify (use sparingly!)"
Write-Host ""
Write-Host "🎉 Happy coding!" -ForegroundColor Green
