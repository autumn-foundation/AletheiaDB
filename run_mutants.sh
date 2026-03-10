#!/bin/bash
cargo mutants --file src/core/temporal.rs --timeout 60 --baseline=skip -- -p aletheiadb --test sentry_temporal > mutant_output_final.txt 2>&1
echo "Mutants exit code: $?"
cat mutant_output_final.txt | grep "MISSED"
