#!/bin/bash
cargo mutants --features "http-server" --file src/http/handlers.rs --test-tool cargo --timeout 60 -- -q --test http_server
