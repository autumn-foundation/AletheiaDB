    /// Helper to sleep for a duration during retries.
    ///
    /// This method replaces blocking sleep calls to avoid stalling async runtimes if possible,
    /// or simply sleeps if running synchronously.
    ///
    /// Note: We deliberately removed tokio::task::block_in_place usage because it can panic
    /// if called from a LocalSet context, which is a crash risk. Dropping locks before sleeping
    /// is the preferred mitigation for DoS risks.
    fn sleep_for_retry(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }

    /// Execute a mutable operation on the index with retry logic for transient errors.
    ///
    /// This method handles acquiring the write lock, executing the operation, and
    /// retrying on transient errors (like thread pool exhaustion). Crucially, it
    /// DROPS the lock before sleeping during backoff to prevent DoS.
    fn execute_with_retry_mut<F, T>(&self, mut op: F, context: &str) -> Result<T>
    where
        F: FnMut(&mut Index) -> std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>,
    {
        for attempt in 0..MAX_SEARCH_ATTEMPTS {
            // Acquire write lock
            let mut index_guard = self.inner.write();

            // Execute operation
            match op(&mut *index_guard) {
                Ok(val) => return Ok(val),
                Err(e) => {
                    let error_msg = e.to_string();

                    // Release lock IMMEDIATELY on error to unblock other threads during sleep
                    drop(index_guard);

                    if is_retryable_usearch_error(&error_msg) && attempt + 1 < MAX_SEARCH_ATTEMPTS {
                        // Track retry
                        self.stats.search_retries.fetch_add(1, Ordering::Relaxed);

                        // Exponential backoff
                        let delay_ms = 1u64 << attempt;
                        self.sleep_for_retry(std::time::Duration::from_millis(delay_ms));
                        continue;
                    }

                    if attempt > 0 {
                        self.stats
                            .search_retry_failures
                            .fetch_add(1, Ordering::Relaxed);
                    }

                    return Err(Error::Vector(VectorError::IndexError(format!(
                        "{}: {}",
                        context, e
                    ))));
                }
            }
        }
        unreachable!("Retry loop should always return")
    }

    /// Helper to retry usearch operations on transient errors (like thread pool exhaustion).
    ///
    /// # Deprecated
    /// This method holds the lock implicitly if the closure captures it.
    /// Use `execute_with_retry_mut` instead for write operations to ensure locks are dropped.
    /// For read operations, manage the lock scope manually with `sleep_for_retry`.
    fn retry_usearch<F, T, E>(&self, mut op: F, context: &str) -> Result<T>
    where
        F: FnMut() -> std::result::Result<T, E>,
        E: std::fmt::Display,
    {
        for attempt in 0..MAX_SEARCH_ATTEMPTS {
            match op() {
                Ok(val) => return Ok(val),
                Err(e) => {
                    let error_msg = e.to_string();
                    if is_retryable_usearch_error(&error_msg) && attempt + 1 < MAX_SEARCH_ATTEMPTS {
                        self.stats.search_retries.fetch_add(1, Ordering::Relaxed);
                        let delay_ms = 1u64 << attempt;
                        self.sleep_for_retry(std::time::Duration::from_millis(delay_ms));
                        continue;
                    }
                    if attempt > 0 {
                        self.stats
                            .search_retry_failures
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    return Err(Error::Vector(VectorError::IndexError(format!(
                        "{}: {}",
                        context, e
                    ))));
                }
            }
        }
        unreachable!("Retry loop should always return")
    }
