#!/bin/bash
cargo mutants --file src/core/graph.rs --timeout 60 -- test --lib core::graph > mutants_verify.log 2>&1
