#!/bin/bash
cargo test -p aletheiadb --lib core::temporal::tests
cargo test -p aletheiadb --lib core::temporal::proptests
