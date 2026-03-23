**Removed Unnecessary .clone() After .into_iter()**
**Learning:** Calling `.into_iter()` on a collection yields owned items, meaning you can move fields directly without needing to clone them. Also, manually pre-allocating a `Vec` and using `.extend()` is an anti-pattern when `.collect()` already uses `size_hint()` perfectly under the hood.
**Action:** Remove `.clone()` when mapping over an owned `into_iter()` stream and trust `.collect()` to pre-allocate correctly.
