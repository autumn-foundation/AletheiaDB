#!/bin/bash
cargo mutants --file src/core/graph.rs --timeout 60 > mutants_graph.log 2>&1
