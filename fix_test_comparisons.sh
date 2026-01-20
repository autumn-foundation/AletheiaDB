#!/bin/bash
# Fix test comparisons with HybridTimestamp

echo "Fixing assert_eq with timestamp fields..."
# Fix assert_eq!(field, NUMBER) patterns
find . -name "*.rs" -type f -exec sed -i -E 's/assert_eq!\(([^,]+\.(start|end|timestamp)), ([0-9]+)\)/assert_eq!(\1, \3.into())/g' {} \;

echo "Fixing assert_eq with explicit comparisons..."
# Fix assert_eq!(field, NUMBER, "message") patterns
find . -name "*.rs" -type f -exec sed -i -E 's/assert_eq!\(([^,]+\.(start|end|timestamp)), ([0-9]+), /assert_eq!(\1, \3.into(), /g' {} \;

echo "Done fixing test comparisons"
