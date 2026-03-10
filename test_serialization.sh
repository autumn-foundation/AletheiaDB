#!/bin/bash
sed -i 's/i64::MAX - 1000/i64::MAX \/ 1000/g' src/core/temporal.rs
cargo test --lib core::temporal::tests::test_serialization_endianness > test_out.txt 2>&1
echo "Test exit code (division): $?"
git checkout src/core/temporal.rs
cat test_out.txt
