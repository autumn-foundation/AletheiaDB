#!/bin/bash
sed -i 's/use aletheiadb::storage::snapshot::StorageSnapshot;//g' src/storage/snapshot.rs
sed -i 's/impl StorageSnapshot for CurrentStorageSnapshot {/impl CurrentStorageSnapshot {/g' src/storage/snapshot.rs
