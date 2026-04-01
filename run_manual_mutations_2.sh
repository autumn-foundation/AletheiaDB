#!/bin/bash
git checkout src/query/parser.rs

sed -i 's/RelationshipDirection::Both/RelationshipDirection::Outgoing/g' src/query/parser.rs
cargo test --lib parser > result_both_outgoing.txt 2>&1
echo "Mutant: Both -> Outgoing Exit code: $?"

git checkout src/query/parser.rs
