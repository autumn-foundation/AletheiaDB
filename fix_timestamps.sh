#!/bin/bash
# Comprehensive timestamp migration fix script

# Fix TimeRange::now calls with literals
find . -name "*.rs" -type f -exec sed -i 's/TimeRange::now(\([0-9]\+\))/TimeRange::now(\1.into())/g' {} \;

# Fix find_similar_as_of with literal timestamps
find . -name "*.rs" -type f -exec sed -i 's/find_similar_as_of(\([^,]*\), \([^,]*\), \([0-9]\+\))/find_similar_as_of(\1, \2, \3.into())/g' {} \;

# Fix find_similar_in_range with literal timestamps in TimeRange
find . -name "*.rs" -type f -exec sed -i 's/TimeRange::new(\([0-9]\+\), \([0-9]\+\))/TimeRange::new(\1.into(), \2.into())/g' {} \;

# Fix query_temporal_range with literal timestamps
find . -name "*.rs" -type f -exec sed -i 's/query_temporal_range(\([^,]*\), \([0-9]\+\), \([0-9]\+\))/query_temporal_range(\1, \2.into(), \3.into())/g' {} \;

# Fix get_snapshots_in_range with literal timestamps
find . -name "*.rs" -type f -exec sed -i 's/get_snapshots_in_range(\([0-9]\+\), \([0-9]\+\))/get_snapshots_in_range(\1.into(), \2.into())/g' {} \;

# Fix query_node_at with literal timestamps
find . -name "*.rs" -type f -exec sed -i 's/query_node_at(\([^,]*\), \([0-9]\+\), \([0-9]\+\))/query_node_at(\1, \2.into(), \3.into())/g' {} \;

# Fix black_box with literal timestamps
find . -name "*.rs" -type f -exec sed -i 's/black_box(\([0-9]\+\))/black_box(\1.into())/g' {} \;

echo "Timestamp fixes applied"
