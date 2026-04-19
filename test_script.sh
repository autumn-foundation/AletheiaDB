#!/bin/bash
# Find single-implementation traits by:
# 1. Finding all `pub trait X` definitions
# 2. Finding all `impl X for Y` implementations
# 3. Finding traits that have exactly ONE implementation

for trait_file in $(find src -name "*.rs" -type f); do
    grep -oP "pub trait \K\w+" "$trait_file" | while read trait_name; do
        # Count implementations
        impl_count=$(grep -r "impl $trait_name for" src/ | wc -l)
        if [ "$impl_count" -eq 1 ]; then
            echo "Single-impl trait: $trait_name in $trait_file"
        fi
    done
done
