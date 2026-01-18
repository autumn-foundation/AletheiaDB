#!/bin/bash
# Final comprehensive cleanup of timestamp migration

echo "Removing duplicate .into() calls..."
# Remove .into().into() patterns
find . -name "*.rs" -type f -exec sed -i 's/\.into()\.into()/.into()/g' {} \;

echo "Fixing broken TimeRange::new calls..."
# Fix TimeRange::new with missing .into()
find . -name "*.rs" -type f -exec sed -i -E 's/TimeRange::new\(([^,]+), ([0-9]+)\)/TimeRange::new(\1, \2.into())/g' {} \;

echo "Fixing as i64 without .into()..."
# Fix (expr) as i64 that should be ((expr) as i64).into() in function calls
find . -name "*.rs" -type f -exec sed -i -E 's/\(([^)]+)\) as i64\)([,;)])/(\1 as i64).into())\2/g' {} \;

echo "Fixing simple integer literals in method calls..."
# Fix index.method(..., INTEGER)
find . -name "*.rs" -type f -exec sed -i -E 's/(index\.(add|remove|on_transaction_at|find_similar_as_of))\(([^,]+), ([^,]+), ([0-9]+)\)/\1(\3, \4, \5.into())/g' {} \;

echo "Final cleanup complete"
