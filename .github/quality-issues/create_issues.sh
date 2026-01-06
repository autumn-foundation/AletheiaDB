#!/usr/bin/env bash
#
# Script to create GitHub issues from markdown templates
# Requires: gh CLI tool (https://cli.github.com/)
#
# Usage: ./create_issues.sh

set -euo pipefail

ISSUES_DIR="$(dirname "$0")"
REPO="madmax983/GallifreyDB"

# Check if gh is installed
if ! command -v gh &> /dev/null; then
    echo "Error: gh CLI is not installed"
    echo "Install it from: https://cli.github.com/"
    exit 1
fi

# Check if authenticated
if ! gh auth status &> /dev/null; then
    echo "Error: Not authenticated with GitHub"
    echo "Run: gh auth login"
    exit 1
fi

echo "Creating GitHub issues for code quality review..."
echo "Repository: $REPO"
echo ""

# Counter for created issues
created=0
failed=0

# Process each issue file
for issue_file in "$ISSUES_DIR"/issue-*.md; do
    if [ ! -f "$issue_file" ]; then
        continue
    fi

    filename=$(basename "$issue_file")
    echo "Processing: $filename"

    # Extract title from frontmatter
    title=$(grep "^title:" "$issue_file" | sed 's/title: "\(.*\)"/\1/')

    # Extract labels from frontmatter
    labels=$(grep "^labels:" "$issue_file" | sed 's/labels: \[\(.*\)\]/\1/' | tr ',' '\n' | sed 's/[" ]//g' | tr '\n' ',' | sed 's/,$//')

    # Extract body (everything after frontmatter)
    body=$(sed '1,/^---$/d; /^---$/d' "$issue_file")

    # Create issue
    if gh issue create \
        --repo "$REPO" \
        --title "$title" \
        --body "$body" \
        --label "$labels" > /dev/null 2>&1; then
        echo "  ✓ Created: $title"
        ((created++))
    else
        echo "  ✗ Failed: $title"
        ((failed++))
    fi

    # Small delay to avoid rate limiting
    sleep 1
done

echo ""
echo "Summary:"
echo "  Created: $created issues"
echo "  Failed: $failed issues"
echo ""
echo "View all issues: https://github.com/$REPO/issues?q=is%3Aissue+is%3Aopen+label%3Acode-quality"
