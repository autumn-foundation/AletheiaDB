#!/bin/bash
git checkout src/query/parser.rs

sed -i 's/start_dir == RelationshipDirection::Incoming/start_dir != RelationshipDirection::Incoming/g' src/query/parser.rs
cargo test --lib parser > result_incoming.txt 2>&1
echo "Mutant: == -> != Exit code: $?"

git checkout src/query/parser.rs
