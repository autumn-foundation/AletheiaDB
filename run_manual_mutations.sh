#!/bin/bash
# We will manually test the mutations on is_relationship_start

sed -i 's/matches!(self.current(), Some(Token::Dash) | Some(Token::LeftArrow))/true/' src/query/parser.rs
cargo test --lib parser > result_true.txt 2>&1
echo "Mutant: true -> Exit code: $?"

git checkout src/query/parser.rs
sed -i 's/matches!(self.current(), Some(Token::Dash) | Some(Token::LeftArrow))/false/' src/query/parser.rs
cargo test --lib parser > result_false.txt 2>&1
echo "Mutant: false -> Exit code: $?"

git checkout src/query/parser.rs
