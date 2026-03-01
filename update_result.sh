#!/bin/bash
sed -i 's/-> Result<Self, String>/-> crate::core::error::Result<Self>/g' src/index/adjacency.rs
sed -i 's/-> std::result::Result<(), String>/-> crate::core::error::Result<()>/g' src/storage/current/mod.rs
sed -i 's/-> Result<(), String>/-> crate::core::error::Result<()>/g' src/index/current.rs
