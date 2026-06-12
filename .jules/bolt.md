**Avoid Unnecessary Vector Collection when creating Iterators**
**Learning:** Calling `.collect::<Vec<_>>().into_iter()` on an iterator chain materializes the entire sequence into a temporary `Vec` on the heap, only to immediately consume it as an iterator.
**Action:** Remove the intermediate `.collect::<Vec<_>>()` to keep the iterator lazy and avoid heap allocations, returning the iterator directly or `Box`ing it if type erasure is needed.
