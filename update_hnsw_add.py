import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

start_idx = 991  # line 992 (0-indexed)
end_idx = 1089   # line 1090 (approx)

# Find the start of Vacant branch
for i, line in enumerate(lines):
    if "dashmap::mapref::entry::Entry::Vacant(entry) => {" in line:
        start_idx = i
        break

# Find the end of Vacant branch
# It should end with `            }` before `        }` before `    }` before `}`
# We look for the closing brace that matches the indentation of `Vacant`.
# Indentation of `Vacant` line is 12 spaces.
for i in range(start_idx + 1, len(lines)):
    if lines[i].startswith("            }"):
        end_idx = i + 1
        break

print(f"Replacing lines {start_idx+1} to {end_idx}")

new_code = """            dashmap::mapref::entry::Entry::Vacant(entry) => {
                // New node: allocate key with overflow protection
                // Check BEFORE incrementing to avoid leaving next_key in invalid state
                const MAX_VALID_KEY: u64 = u64::MAX - 1000;

                // CRITICAL: Drop the entry to release DashMap lock BEFORE acquiring inner lock
                // This prevents lock ordering inversion (dashmap -> inner is FORBIDDEN)
                drop(entry);

                let mut collisions = 0;
                loop {
                    // Step 1: Atomically allocate a unique key (no locks held)
                    let key = loop {
                        let current = self.next_key.load(Ordering::SeqCst);
                        if current > MAX_VALID_KEY {
                            return Err(Error::Vector(VectorError::IndexError(
                                "Maximum number of vectors exceeded (key overflow protection)"
                                    .to_string(),
                            )));
                        }
                        // Try to atomically increment; retry if another thread beat us
                        match self.next_key.compare_exchange(
                            current,
                            current + 1,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(key) => break key,
                            Err(_) => continue, // Retry with new current value
                        }
                    };

                    // Note: save_lock is already held at start of function.

                    // Step 2: Acquire inner write lock FIRST (follows lock ordering invariant).
                    // Vacant path updates the index before claiming the map entry (Inner -> Map).
                    let index = self.inner.write();

                    // Ensure capacity exists. The optimistic check in check_and_expand_capacity
                    // might have been raced by other threads. Since we hold the write lock now,
                    // we are the source of truth.
                    if index.size() >= index.capacity() {
                        let new_capacity = (index.capacity() * 2).max(1024);
                        if let Err(e) = self.retry_usearch(
                            || index.reserve(new_capacity),
                            "Failed to expand capacity (race recovery)",
                        ) {
                            return Err(e);
                        }
                    }

                    // Step 3: Add to inner usearch index while holding write lock
                    // Handle "Duplicate keys" error by retrying with new key (Robustness fix)
                    match self.retry_usearch(|| index.add(key, vector), "Failed to add vector") {
                        Ok(_) => {}, // Success, continue
                        Err(e) => {
                            if e.to_string().contains("Duplicate keys") {
                                // Collision detected! (Likely due to persistence mismatch)
                                // Drop lock, increment retry count, and continue loop to get new key
                                drop(index);
                                collisions += 1;
                                if collisions > 1000 {
                                    return Err(Error::Vector(VectorError::IndexError(
                                        "Too many key collisions (index corruption likely)".to_string()
                                    )));
                                }
                                continue; // Loop again, get new key
                            }
                            return Err(e); // Propagate other errors
                        }
                    }

                    #[cfg(test)]
                    {
                        // Hook to simulate race condition: simulate another thread adding mapping
                        // after we checked it was vacant but before we inserted our mapping.
                        if let Some(hook) = TEST_RACE_HOOK.with(|h| h.get()) {
                            hook(self, id);
                        }
                    }

                    // Step 4: Insert to mappings (dashmap) WHILE HOLDING INNER LOCK
                    // We keep the inner lock held to ensure atomicity with respect to save_internal().
                    // If we dropped the lock here, save_internal() could run, see the new vector in inner,
                    // but miss the mapping in id_mapping (Zombie Vector bug).
                    //
                    // Lock order check: Inner -> Map (via entry()). This is consistent with other operations.
                    let race_detected = match self.id_mapping.entry(id) {
                        dashmap::mapref::entry::Entry::Occupied(_) => true,
                        dashmap::mapref::entry::Entry::Vacant(e) => {
                            // Success: we claimed the ID
                            e.insert(key);
                            // Drop the entry lock implicitly here when e is consumed/scope ends
                            false
                        }
                    };

                    if race_detected {
                        // Race detected: Another thread added this NodeId concurrently
                        // Our vector is in inner with key=key, but someone else claimed the ID.
                        // We must rollback our addition to avoid phantom vectors.

                        // We already hold the inner write lock, so we can remove directly.
                        if let Err(e) = self.retry_usearch(
                            || index.remove(key),
                            "Failed to rollback vector after concurrent add",
                        ) {
                            return Err(e);
                        }

                        // The existing mapping wins; return error to indicate retry needed
                        return Err(Error::Vector(VectorError::IndexError(
                            "Concurrent add detected for same NodeId, vector already exists"
                                .to_string(),
                        )));
                    }

                    // If no race, we successfully inserted into id_mapping.
                    // Now insert reverse mapping.
                    self.reverse_mapping.insert(key, id);
                    self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);

                    // Explicitly drop index lock (though it would drop at end of scope)
                    drop(index);

                    return Ok(());
                }
            }
"""

lines[start_idx:end_idx] = [new_code + "\n"]

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
