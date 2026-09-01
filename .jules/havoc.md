**[WalRingBuffer Race Condition Fix]
**Trigger:** Concurrent drains/reads mixed with writes causing integer underflow in `len_approx()`.
**Action:** Reordered atomic loads in `len_approx()` so `read_pos` is read BEFORE `write_pos`, and clamped the result with `.min(self.capacity as u64)`. Applied this bounded capacity to `drain()` pre-allocation (`Vec::with_capacity(approx_len)`) to safely optimize performance without causing memory bloat or OOM crashes.
