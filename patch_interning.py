with open("src/core/interning.rs", "r") as f:
    content = f.read()

target = """    pub fn resolve(&self, id: InternedString) -> Option<Arc<str>> {"""
replacement = """    /// Resolves an `InternedString` handle back into its original string value.
    ///
    /// # The Details
    /// Because an `InternedString` is just a lightweight `u32` identifier, it doesn't contain
    /// the string bytes itself. This method looks up the original `Arc<str>` in the global
    /// string registry.
    ///
    /// # Return
    /// Returns `Some(Arc<str>)` if the string ID exists in the interner, or `None` if it does not.
    ///
    /// # Examples
    /// ```rust
    /// use aletheiadb::core::interning::StringInterner;
    ///
    /// let interner = StringInterner::new();
    /// let id = interner.intern("hello");
    ///
    /// // Retrieve the original string
    /// let text = interner.resolve(id).unwrap();
    /// assert_eq!(&*text, "hello");
    /// ```
    pub fn resolve(&self, id: InternedString) -> Option<Arc<str>> {"""

content = content.replace(target, replacement)

with open("src/core/interning.rs", "w") as f:
    f.write(content)
