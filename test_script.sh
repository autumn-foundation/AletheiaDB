#!/bin/bash
cargo mutants --file src/core/temporal.rs --timeout 60 --baseline=skip --list | grep "116:47" > mutants.txt
cat mutants.txt
