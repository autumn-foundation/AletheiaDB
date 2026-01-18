#!/bin/bash
# Comprehensive fix for ALL remaining timestamp errors

echo "Fixing (X as i64) patterns..."
# Fix patterns like: function_call(args, (expr) as i64)
find . -name "*.rs" -type f -exec sed -i -E 's/\(([^)]+)\) as i64\)/(\1 as i64).into()/g' {} \;

echo "Fixing X as i64 patterns without parens..."
# Fix patterns like: function_call(args, expr as i64)
find . -name "*.rs" -type f -exec sed -i -E 's/([^(])([0-9]+) as i64\)/\1(\2 as i64).into()/g' {} \;

echo "Fixing remaining .add/.remove patterns with as i64..."
# Specific fix for method calls with as i64
find . -name "*.rs" -type f -exec sed -i -E 's/(\.add|\.remove)\(([^,]+), ([^,]+), ([^)]+) as i64\)/\1(\2, \3, (\4 as i64).into())/g' {} \;

echo "Fixing usize .into() errors..."
# Remove .into() from usize comparisons
find . -name "*.rs" -type f -exec sed -i -E 's/assert_eq!\(([^,]+\.len\(\)), ([0-9]+)\.into\(\)\)/assert_eq!(\1, \2)/g' {} \;
find . -name "*.rs" -type f -exec sed -i -E 's/assert_eq!\(([^,]+_count\(\)), ([0-9]+)\.into\(\)\)/assert_eq!(\1, \2)/g' {} \;

echo "Fixing timestamp variable declarations..."
# Fix let timestamp = expr as i64;
find . -name "*.rs" -type f -exec sed -i -E 's/let ([a-z_]+) = ([^;]+) as i64;/let \1 = (\2 as i64).into();/g' {} \;

echo "Fixing TimeRange::new with as i64..."
# Fix TimeRange::new(X as i64, Y as i64)
find . -name "*.rs" -type f -exec sed -i -E 's/TimeRange::new\(([^,]+) as i64, ([^)]+) as i64\)/TimeRange::new((\1 as i64).into(), (\2 as i64).into())/g' {} \;

echo "Fixing BiTemporalInterval::now patterns..."
find . -name "*.rs" -type f -exec sed -i -E 's/BiTemporalInterval::now\(([^,]+) as i64, ([^)]+) as i64\)/BiTemporalInterval::now((\1 as i64).into(), (\2 as i64).into())/g' {} \;

echo "All fixes applied!"
