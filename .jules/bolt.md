**Avoid intermediate Vec allocations when parsing queries**
**Learning:** `cleaned.split_whitespace().collect::<Vec<_>>().join(" ");` creates an unnecessary `Vec` allocation on the heap, which isn't necessary for simple string transformations.
**Action:** Use a pre-allocated string with `String::with_capacity(cleaned.len())` and a loop over the word iterator to concatenate spaces and words directly.
