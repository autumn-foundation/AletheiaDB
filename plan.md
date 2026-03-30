👺 Havoc: Loom Model for GroupCommitCoordinator Deadlocks

🧨 The Trigger:
High-concurrency Group Commit operations (registering, starting flushes, waiting for `flush_complete` Condvar timeouts, and finishing flushes) could theoretically deadlock if spurious wakeups or incorrect lock acquisition orders are hit, especially around `flushed_epoch` transitions and `completed_epochs` set modifications.

📉 The Stack Trace:
(N/A - Deadlocks freeze the threads in Loom without a stack trace unless explicitly panicked, but the intent was to ensure no threads hang indefinitely during epoch resolution)

🧪 Reproduction:
Run the loom model directly:
`RUSTFLAGS="--cfg loom" cargo test --test havoc test_group_commit_coordinator_deadlock`

😈 Comment:
"I tried to break your little group commit epochs. I assumed that a thread timing out on wait_for_flush while another thread successfully finished the flush would result in a lost wakeup or poison the lock. You survived this one."
