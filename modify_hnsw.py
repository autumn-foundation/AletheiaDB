import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

new_method = """    fn save_internal(&self, path: &Path) -> Result<()> {
        // Generate unique suffix for atomic save (timestamp + random if needed, but timestamp is usually enough)
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let suffix = format!("{}", timestamp);

        let index_tmp_path = path.with_extension(format!("usearch.{}.tmp", suffix));
        let mappings_tmp_path = path.with_extension(format!("usearch.mappings.{}.tmp", suffix));
        let mappings_path = path.with_extension("usearch.mappings");

        // Acquire save_lock (exclusive) to prevent concurrent adds/removes.
        // This ensures consistency between the index snapshot and the ID mappings.
        let _save_guard = self.save_lock.write();

        // Acquire inner write lock to serialize  with concurrent searches.
        let index = self.inner.write();

        // Collect mappings while holding the locks.
        // Memory cost: O(N) allocation (~16MB for 1M nodes), acceptable for infrequent save operation.
        let mappings: Vec<(NodeId, u64)> = self
            .id_mapping
            .iter()
            .map(|e| (*e.key(), *e.value()))
            .collect();
        let count = mappings.len();

        // Save usearch index to temporary file
        index
            .save(index_tmp_path.to_str().ok_or_else(|| {
                Error::Vector(VectorError::IndexError(
                    "Path contains invalid UTF-8".to_string(),
                ))
            })?)
            .map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to save index to temp file: {}",
                    e
                )))
            })?;

        // Explicit drop to release lock before I/O (mappings write)
        drop(index);
        // We drop save_lock here to allow concurrency.
        // Temp files are unique, so no conflict.
        drop(_save_guard);

        // Calculate expected size for integrity check (logic preserved)
        let count_size = count
            .checked_mul(16)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;
        let _total_size = count_size
            .checked_add(4 + 1 + 8 + 1 + 1 + 8 + 4)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;

        // Save mappings to temporary companion file
        let file = File::create(&mappings_tmp_path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create temp mappings file: {}",
                e
            )))
        })?;
        let mut writer = BufWriter::new(file);

        // Use Vec iterator instead of DashMap iterator
        Self::write_mappings_to_writer(&mut writer, mappings.into_iter(), count, &self.config)?;

        // Atomic Rename Sequence: Mappings FIRST, then Index
        // This ensures that if we crash, we might have (New Mappings, Old Index)
        // which prevents key collision on recovery (next_key will be high).
        // If we did Index first, we could have (New Index, Old Mappings),
        // causing next_key to be low, leading to collisions.

        std::fs::rename(&mappings_tmp_path, &mappings_path).map_err(|e| {
            // Attempt to clean up temp files on error
            let _ = std::fs::remove_file(&index_tmp_path);
            let _ = std::fs::remove_file(&mappings_tmp_path);
            Error::Vector(VectorError::IndexError(format!(
                "Failed to rename mappings file: {}",
                e
            )))
        })?;

        std::fs::rename(&index_tmp_path, path).map_err(|e| {
            // Attempt to clean up temp files on error
            let _ = std::fs::remove_file(&index_tmp_path);
            // Note: mappings file is already renamed, so we can't rollback easily without risk.
            // But we are in "New Mappings, Old Index" state, which is safe for collision.
            Error::Vector(VectorError::IndexError(format!(
                "Failed to rename index file: {}",
                e
            )))
        })?;

        Ok(())
    }
"""

start_line = 1611 # 0-indexed, so 1612 -> 1611
end_line = 1667   # Replaces up to write_mappings_to_writer

# Find start of save_internal
for i, line in enumerate(lines):
    if "fn save_internal" in line:
        start_line = i
        break

# Find start of write_mappings_to_writer
for i, line in enumerate(lines):
    if i > start_line and "fn write_mappings_to_writer" in line:
        end_line = i
        break

# Find the end of save_internal (just before write_mappings_to_writer helper method comment)
# The previous lines has "}" closing the function.
# Look backwards from end_line
replace_end = end_line
while replace_end > start_line:
    if "/// Helper method" in lines[replace_end-1]:
        replace_end -= 1
        break
    replace_end -= 1

# Replace lines
lines[start_line:replace_end] = [new_method + "\n"]

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
