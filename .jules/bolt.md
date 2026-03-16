**Avoid intermediate Vec allocations when parsing queries**
**Learning:** `cleaned.split_whitespace().collect::<Vec<_>>().join(" ");` creates an unnecessary `Vec` allocation on the heap, which isn't necessary for simple string transformations.
**Action:** Use a pre-allocated string with `String::with_capacity(cleaned.len())` and a loop over the word iterator to concatenate spaces and words directly.

**[Unused Doc Comments in Function Bodies]
**Learning:** Using `///` doc comments inside a function body that do not attach to an item (e.g., before a `let` statement or loop) triggers the `clippy::unused_doc_comments` warning.
**Action:** Use standard `//` comments for internal inline explanations, saving `///` exclusively for struct, enum, trait, and function definitions.
