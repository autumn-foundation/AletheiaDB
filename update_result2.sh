#!/bin/bash
sed -i 's/-> std::result::Result<(), String>/-> crate::core::error::Result<()>/g' src/storage/current/mod.rs
sed -i 's/return Err(format!(/return Err(crate::core::error::StorageError::CorruptedData(format!(/g' src/index/current.rs
sed -i 's/incoming_tombstones/incoming_tombstones\n            )).into());/g' src/index/current.rs
