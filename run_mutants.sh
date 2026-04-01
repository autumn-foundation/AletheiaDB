#!/bin/bash
cargo mutants --file src/query/parser.rs -- --test parser
