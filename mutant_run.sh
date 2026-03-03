#!/bin/bash
cargo mutants -j 4 --timeout 30 -f src/query/planner/rules/operation_reordering.rs -- -p aletheiadb --lib query::planner::rules::operation_reordering
