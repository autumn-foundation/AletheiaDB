#!/usr/bin/env bash
# Setup pre-commit hooks for AletheiaDB
set -e

echo "🔧 Setting up pre-commit hooks for AletheiaDB..."

# Check if pre-commit is installed
if ! command -v pre-commit &> /dev/null; then
    echo "📦 Installing pre-commit..."
    if command -v pip3 &> /dev/null; then
        pip3 install pre-commit
    elif command -v pip &> /dev/null; then
        pip install pre-commit
    else
        echo "❌ Error: pip not found. Please install Python and pip first."
        echo "Visit: https://pip.pypa.io/en/stable/installation/"
        exit 1
    fi
fi

# Check if cargo-audit is installed
if ! command -v cargo-audit &> /dev/null; then
    echo "📦 Installing cargo-audit..."
    cargo install cargo-audit
fi

# Check if cargo-edit is installed (for version bumping)
if ! command -v cargo-set-version &> /dev/null; then
    echo "📦 Installing cargo-edit..."
    cargo install cargo-edit
fi

# Install pre-commit hooks
echo "⚙️  Installing pre-commit hooks..."
pre-commit install --hook-type pre-commit --hook-type commit-msg --hook-type pre-push

# Run hooks on all files to verify setup
echo "✅ Running pre-commit on all files to verify setup..."
pre-commit run --all-files || {
    echo ""
    echo "⚠️  Some hooks failed, but that's okay for initial setup!"
    echo "   Run 'cargo fmt' and 'cargo clippy --fix' to fix issues."
}

echo ""
echo "✅ Pre-commit hooks successfully installed!"
echo ""
echo "📝 Hooks installed:"
echo "   - pre-commit: Runs on 'git commit' (format, clippy, checks)"
echo "   - commit-msg: Enforces conventional commits"
echo "   - pre-push: Runs tests and security audit before push"
echo ""
echo "💡 Tips:"
echo "   - Run hooks manually: pre-commit run --all-files"
echo "   - Update hooks: pre-commit autoupdate"
echo "   - Skip hooks: git commit --no-verify (use sparingly!)"
echo ""
echo "🎉 Happy coding!"
