#!/bin/bash
cargo mutants --file src/core/id.rs --list > mutants.txt
cat mutants.txt
