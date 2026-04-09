#!/bin/bash
sed -i 's/let dir = tempdir().unwrap();/let config = CheckpointConfig::with_data_dir(\&dir_path_clone);/g' tests/snapshot_race_condition.rs
sed -i 's/let config = CheckpointConfig::with_data_dir(dir.path());//g' tests/snapshot_race_condition.rs
