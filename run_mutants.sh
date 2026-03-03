#!/bin/bash
mutants=(
    "src/core/id.rs:1494:24"
    "src/core/id.rs:1509:9"
    "src/core/id.rs:264:15"
    "src/core/id.rs:411:25"
)

for m in "${mutants[@]}"; do
    echo "Running mutant: $m"
    cargo mutants --in-place --timeout 30 -E "$m" --test-tool cargo -- --lib core::id::
done
