**👺 Havoc: Lock Ordering Deadlock in BatchBuffer**

🧨 **The Trigger:**
Calling `is_timeout_expired()` on `BatchBuffer` concurrently with `add()` or `flush()`.

📉 **The Stack Trace:**
(Simulated deadlock, no direct panic trace but thread hangs indefinitely waiting on Mutex).
Deadlocks happen when Thread A locks `events` then tries to lock `batch_start` (in `add()` or `flush()`), while Thread B calls `is_timeout_expired()` which locks `batch_start` and then might be interrupted or interleaved with another operation that subsequently tries to lock `events` (e.g. `len()`).

🧪 **Reproduction:**
Create a concurrent scenario where one thread adds events, while another thread repeatedly calls `is_timeout_expired()` and `len()`. The lock inversion (locking `batch_start` without holding `events`) violates the explicit struct invariants and leads to a classic AB-BA deadlock.

😈 **Comment:**
"You wrote the lock ordering rules in your own struct documentation but forgot to follow them in your newest method. The rules apply to everyone, even you."
