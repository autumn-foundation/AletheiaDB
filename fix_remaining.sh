#!/bin/bash
# Final comprehensive timestamp fixes

# Fix all add/remove/on_transaction_at patterns that might have been missed
find . -name "*.rs" -type f -exec sed -i -E 's/\.(add|remove|on_transaction_at|find_similar_as_of)\(([^)]*), ([0-9]+)\)/.\1(\2, \3.into())/g' {} \;

# Fix BiTemporalInterval::now calls
find . -name "*.rs" -type f -exec sed -i -E 's/BiTemporalInterval::now\(([0-9]+), ([0-9]+)\)/BiTemporalInterval::now(\1.into(), \2.into())/g' {} \;

# Fix assert_eq with .wallclock() comparisons
find . -name "*.rs" -type f -exec sed -i -E 's/assert_eq!\(([^,]+)\.wallclock\(\), ([0-9]+)\.into\(\)\)/assert_eq!(\1.wallclock(), \2)/g' {} \;

# Fix TimeRange patterns in various contexts
find . -name "*.rs" -type f -exec sed -i -E 's/TimeRange\(([0-9]+), ([0-9]+)\)/TimeRange(\1.into(), \2.into())/g' {} \;

echo "Final timestamp fixes applied"
echo "Remaining errors should be manually reviewed"
