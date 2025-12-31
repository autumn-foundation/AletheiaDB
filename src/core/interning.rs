//! String interning for memory efficiency.
//!
//! This module provides a thread-safe string interner that maps strings to small
//! integer IDs. This is particularly useful for labels and property keys that are
//! repeated many times throughout the database.
//!
//! Benefits:
//! - Reduces memory usage (4 bytes instead of 24 for each string reference)
//! - Enables O(1) string equality checks (compare u32 instead of string contents)
//! - Thread-safe without locking (uses DashMap)

use dashmap::DashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// A small, copyable handle to an interned string.
///
/// This is just a u32 ID that can be used to look up the original string
/// in the interner. It's Copy, so passing it around is very cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InternedString(u32);

impl InternedString {
    /// Create an InternedString from a raw u32 ID.
    ///
    /// # Safety
    /// This is safe but the caller must ensure the ID is valid in the interner.
    #[inline]
    pub const fn from_raw(id: u32) -> Self {
        InternedString(id)
    }

    /// Get the raw u32 ID.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for InternedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Interned({})", self.0)
    }
}

/// Thread-safe string interner.
///
/// This interner maintains a bidirectional mapping between strings and IDs:
/// - String → ID: For interning new strings
/// - ID → String: For resolving interned strings
///
/// The interner is designed to be used as a singleton (via lazy_static or similar).
pub struct StringInterner {
    /// Maps strings to their IDs.
    string_to_id: DashMap<Arc<str>, InternedString>,
    /// Maps IDs back to strings.
    id_to_string: DashMap<InternedString, Arc<str>>,
    /// Next ID to assign.
    next_id: AtomicU32,
}

impl StringInterner {
    /// Create a new empty string interner.
    pub fn new() -> Self {
        StringInterner {
            string_to_id: DashMap::new(),
            id_to_string: DashMap::new(),
            next_id: AtomicU32::new(0),
        }
    }

    /// Intern a string, returning its ID.
    ///
    /// If the string was already interned, returns the existing ID.
    /// Otherwise, assigns a new ID and stores the string.
    ///
    /// This method is thread-safe and lock-free.
    pub fn intern<S: AsRef<str>>(&self, string: S) -> InternedString {
        let string = string.as_ref();

        // Fast path: check if already interned
        if let Some(id) = self.string_to_id.get(string) {
            return *id;
        }

        // Slow path: need to intern a new string
        let arc_str: Arc<str> = Arc::from(string);

        // Double-check to handle race conditions
        // Another thread might have interned it while we were creating the Arc
        if let Some(id) = self.string_to_id.get(arc_str.as_ref()) {
            return *id;
        }

        // Assign a new ID
        let id = InternedString(self.next_id.fetch_add(1, Ordering::Relaxed));

        // Store in both directions
        self.string_to_id.insert(Arc::clone(&arc_str), id);
        self.id_to_string.insert(id, arc_str);

        id
    }

    /// Resolve an interned string ID back to the original string.
    ///
    /// Returns None if the ID is not valid (was never interned).
    pub fn resolve(&self, id: InternedString) -> Option<Arc<str>> {
        self.id_to_string.get(&id).map(|entry| Arc::clone(entry.value()))
    }

    /// Get the string as a &str without cloning the Arc.
    ///
    /// This is useful when you just need to read the string temporarily.
    pub fn get(&self, id: InternedString) -> Option<impl AsRef<str> + '_> {
        self.id_to_string.get(&id).map(|entry| {
            let arc: Arc<str> = Arc::clone(entry.value());
            arc
        })
    }

    /// Check if a string has been interned.
    pub fn contains<S: AsRef<str>>(&self, string: S) -> bool {
        self.string_to_id.contains_key(string.as_ref())
    }

    /// Get the ID of a string if it has been interned.
    pub fn get_id<S: AsRef<str>>(&self, string: S) -> Option<InternedString> {
        self.string_to_id.get(string.as_ref()).map(|entry| *entry.value())
    }

    /// Get the number of interned strings.
    pub fn len(&self) -> usize {
        self.string_to_id.len()
    }

    /// Check if the interner is empty.
    pub fn is_empty(&self) -> bool {
        self.string_to_id.is_empty()
    }

    /// Clear all interned strings.
    ///
    /// This invalidates all existing InternedString IDs!
    pub fn clear(&self) {
        self.string_to_id.clear();
        self.id_to_string.clear();
        self.next_id.store(0, Ordering::Relaxed);
    }
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

/// Global string interner instance.
///
/// This can be used throughout the application for interning labels,
/// property keys, and other frequently repeated strings.
///
/// # Example
///
/// ```ignore
/// use gallifreydb::core::interning::GLOBAL_INTERNER;
///
/// let id1 = GLOBAL_INTERNER.intern("Person");
/// let id2 = GLOBAL_INTERNER.intern("Person");
/// assert_eq!(id1, id2); // Same string gets same ID
///
/// let string = GLOBAL_INTERNER.resolve(id1).unwrap();
/// assert_eq!(string.as_ref(), "Person");
/// ```
use std::sync::LazyLock;
pub static GLOBAL_INTERNER: LazyLock<StringInterner> = LazyLock::new(StringInterner::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_same_string() {
        let interner = StringInterner::new();

        let id1 = interner.intern("hello");
        let id2 = interner.intern("hello");

        assert_eq!(id1, id2, "Same string should get same ID");
    }

    #[test]
    fn test_intern_different_strings() {
        let interner = StringInterner::new();

        let id1 = interner.intern("hello");
        let id2 = interner.intern("world");

        assert_ne!(id1, id2, "Different strings should get different IDs");
    }

    #[test]
    fn test_resolve() {
        let interner = StringInterner::new();

        let id = interner.intern("test");
        let resolved = interner.resolve(id).expect("Should resolve");

        assert_eq!(resolved.as_ref(), "test");
    }

    #[test]
    fn test_resolve_invalid_id() {
        let interner = StringInterner::new();

        let invalid_id = InternedString::from_raw(999);
        assert!(interner.resolve(invalid_id).is_none());
    }

    #[test]
    fn test_contains() {
        let interner = StringInterner::new();

        assert!(!interner.contains("test"));

        interner.intern("test");

        assert!(interner.contains("test"));
        assert!(!interner.contains("other"));
    }

    #[test]
    fn test_get_id() {
        let interner = StringInterner::new();

        assert_eq!(interner.get_id("test"), None);

        let id = interner.intern("test");

        assert_eq!(interner.get_id("test"), Some(id));
    }

    #[test]
    fn test_len() {
        let interner = StringInterner::new();

        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());

        interner.intern("a");
        interner.intern("b");
        interner.intern("a"); // Duplicate, shouldn't increase count

        assert_eq!(interner.len(), 2);
        assert!(!interner.is_empty());
    }

    #[test]
    fn test_clear() {
        let interner = StringInterner::new();

        let id = interner.intern("test");
        assert!(interner.resolve(id).is_some());

        interner.clear();

        assert_eq!(interner.len(), 0);
        assert!(interner.resolve(id).is_none());
    }

    #[test]
    fn test_concurrent_interning() {
        use std::thread;

        let interner = Arc::new(StringInterner::new());
        let mut handles = vec![];

        // Spawn 10 threads, each interning the same strings
        for _ in 0..10 {
            let interner_clone = Arc::clone(&interner);
            let handle = thread::spawn(move || {
                let id1 = interner_clone.intern("concurrent");
                let id2 = interner_clone.intern("test");
                (id1, id2)
            });
            handles.push(handle);
        }

        // Collect all results
        let results: Vec<_> = handles.into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        // All threads should have gotten the same IDs
        let (first_id1, first_id2) = results[0];
        for (id1, id2) in results.iter().skip(1) {
            assert_eq!(*id1, first_id1);
            assert_eq!(*id2, first_id2);
        }

        // Should only have 2 unique strings
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn test_interned_string_size() {
        use std::mem::size_of;

        // InternedString should be just 4 bytes (u32)
        assert_eq!(size_of::<InternedString>(), 4);

        // Compare to a String which is 24 bytes
        assert_eq!(size_of::<String>(), 24);

        // This demonstrates the memory savings
        println!("InternedString: {} bytes", size_of::<InternedString>());
        println!("String: {} bytes", size_of::<String>());
    }

    #[test]
    fn test_global_interner() {
        // Clear to ensure clean state
        GLOBAL_INTERNER.clear();

        let id1 = GLOBAL_INTERNER.intern("global");
        let id2 = GLOBAL_INTERNER.intern("global");

        assert_eq!(id1, id2);

        let resolved = GLOBAL_INTERNER.resolve(id1).unwrap();
        assert_eq!(resolved.as_ref(), "global");
    }
}
