#!/bin/bash
cargo test --lib core::property::tests::test_deserialize_string_overflow || true
