#!/bin/bash

# Warden Security Audit Script
# This script checks if 'cargo-audit' is available and runs it.
# If not available, it prints a warning.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "🔒 Warden Security Audit"
echo "======================"

if command -v cargo-audit &> /dev/null; then
    echo -e "${GREEN}✅ cargo-audit found. Running audit...${NC}"
    if cargo audit; then
        echo -e "${GREEN}✅ Security audit passed.${NC}"
        exit 0
    else
        echo -e "${RED}❌ Security audit failed! Please fix vulnerabilities.${NC}"
        exit 1
    fi
else
    echo -e "${YELLOW}⚠️  cargo-audit NOT found.${NC}"
    echo "To install: cargo install cargo-audit"
    echo "Skipping automated audit."
    # We exit 0 (success) because we don't want to break CI/builds if the tool isn't installed,
    # unless this is a strict environment.
    exit 0
fi
