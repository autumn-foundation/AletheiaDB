#!/bin/bash

# A script to replace db.enable_vector_index(prop, config) with db.vector_index(prop).hnsw(config).enable()
# Note: we will use sed

# Just handling the simple single-line cases:
# db.enable_vector_index("...", config) -> db.vector_index("...").hnsw(config).enable()
# db.enable_vector_index("...", HnswConfig::new(...)) -> db.vector_index("...").hnsw(HnswConfig::new(...)).enable()

find src -name "*.rs" -exec sed -i -e 's/db\.enable_vector_index(\([^,]*\),\s*\(HnswConfig::new[^)]*)\))/db.vector_index(\1).hnsw(\2).enable()/g' {} +
find src -name "*.rs" -exec sed -i -e 's/db\.enable_vector_index(\([^,]*\),\s*\(config[0-9]*\))/db.vector_index(\1).hnsw(\2).enable()/g' {} +

# For mcp server
sed -i 's/self.db.enable_vector_index(&req.property_name, config)/self.db.vector_index(\&req.property_name).hnsw(config).enable()/' src/mcp/server.rs
