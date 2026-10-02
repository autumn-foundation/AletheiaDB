**[MigrationService start/stop Deadlock]**
**The Trigger:** A race condition between `stop()` and `start()` in `MigrationService`. If `start()` fires and overwrites `worker_handle` while `stop()` is waiting on the shutdown condition variable, `stop()` will subsequently extract the *newly spawned* thread's handle and attempt to `.join()` it. Because the new thread is running indefinitely, `stop()` blocks forever.
**The Fix:** Acquired the `worker_handle` mutex lock at the absolute beginning of both `start()` and `stop()`. This fully serializes the lifecycle operations, preventing the start/stop state from interleaving or mutating while the other is executing.
**The Test:** Wrote `test_migration_start_stop_race` which bombards `start()` and `stop()` across two threads concurrently.
