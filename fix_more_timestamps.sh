#!/bin/bash
# More timestamp fixes

# Fix .find_similar_as_of calls
find . -name "*.rs" -type f -exec sed -i 's/\.find_similar_as_of(\([^,]*\), \([^,]*\), \([0-9][0-9]*\))/.find_similar_as_of(\1, \2, \3.into())/g' {} \;

# Fix .get_node_at_time calls
find . -name "*.rs" -type f -exec sed -i 's/\.get_node_at_time(\([^,]*\), \([0-9][0-9]*\), \([0-9][0-9]*\))/.get_node_at_time(\1, \2.into(), \3.into())/g' {} \;

# Fix .get_edge_at_time calls
find . -name "*.rs" -type f -exec sed -i 's/\.get_edge_at_time(\([^,]*\), \([0-9][0-9]*\), \([0-9][0-9]*\))/.get_edge_at_time(\1, \2.into(), \3.into())/g' {} \;

# Fix snapshot.at calls
find . -name "*.rs" -type f -exec sed -i 's/snapshot\.at(\([0-9][0-9]*\))/snapshot.at(\1.into())/g' {} \;

# Fix create_snapshot calls
find . -name "*.rs" -type f -exec sed -i 's/create_snapshot(\([0-9][0-9]*\))/create_snapshot(\1.into())/g' {} \;

# Fix more TimeRange::from patterns
find . -name "*.rs" -type f -exec sed -i 's/TimeRange::from(\([0-9][0-9]*\))/TimeRange::from(\1.into())/g' {} \;

echo "More timestamp fixes applied"
