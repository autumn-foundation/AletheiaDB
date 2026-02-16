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
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Default maximum number of interned strings (DoS protection)
pub const DEFAULT_MAX_INTERNED_STRINGS: usize = 100_000;

/// Common strings that are pre-interned at startup for optimal performance.
///
/// These strings represent frequently used property keys and labels across
/// typical graph database usage patterns. Pre-interning them eliminates the
/// initial allocation overhead on first access.
pub const COMMON_STRINGS: &[&str] = &[
    "name",
    "id",
    "type",
    "label",
    "created_at",
    "updated_at",
    "valid_from",
    "valid_to",
    "tx_from",
    "tx_to",
];

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
        // Try to resolve using the global interner first
        // This makes debugging much easier by showing the actual string
        // We use resolve_with to avoid allocating a new String just for display
        let result = GLOBAL_INTERNER.resolve_with(*self, |s| write!(f, "{}", s));

        // If resolution failed (e.g. ID from a local interner), fallback to showing ID
        match result {
            Some(res) => res,
            None => write!(f, "Interned({})", self.0),
        }
    }
}

/// A hasher that passes through u32 values unchanged.
/// Used for the ID -> String map where keys are already unique integers.
/// This avoids the overhead of hashing (SipHash) for lookups.
#[derive(Default)]
struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    fn write(&mut self, bytes: &[u8]) {
        // Fallback: treat bytes as little-endian u32 if length matches
        if let Ok(bytes) = bytes.try_into() {
            self.0 = u32::from_le_bytes(bytes) as u64;
        } else {
            // Should not happen for InternedString keys
            self.0 = bytes.len() as u64;
        }
    }

    fn write_u32(&mut self, i: u32) {
        self.0 = i as u64;
    }

    fn finish(&self) -> u64 {
        self.0
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
    /// Uses IdentityHasher for O(1) lookups without hashing overhead.
    id_to_string: DashMap<InternedString, Arc<str>, BuildHasherDefault<IdentityHasher>>,
    /// Next ID to assign.
    next_id: AtomicU32,
    /// Maximum number of strings to intern (DoS protection)
    max_capacity: usize,
}

impl StringInterner {
    /// Create a new empty string interner with default capacity limit.
    pub fn new() -> Self {
        Self::with_max_capacity(DEFAULT_MAX_INTERNED_STRINGS)
    }

    /// Create a new string interner with a custom maximum capacity.
    pub fn with_max_capacity(max_capacity: usize) -> Self {
        StringInterner {
            string_to_id: DashMap::new(),
            id_to_string: DashMap::with_hasher(BuildHasherDefault::default()),
            next_id: AtomicU32::new(0),
            max_capacity,
        }
    }

    /// Intern a string, returning its ID.
    ///
    /// If the string was already interned, returns the existing ID.
    /// Otherwise, assigns a new ID and stores the string.
    ///
    /// This method is thread-safe and lock-free.
    ///
    /// # Errors
    /// Returns `Error::Storage(StorageError::CapacityExceeded)` if the maximum
    /// capacity is exceeded (DoS protection). This prevents unbounded memory growth.
    pub fn intern<S: AsRef<str>>(
        &self,
        string: S,
    ) -> std::result::Result<InternedString, crate::utils::error::Error> {
        let string = string.as_ref();

        // Fast path: check if already interned (avoids Arc allocation)
        if let Some(id) = self.get_id(string) {
            return Ok(id);
        }

        // Slow path: need to intern the string
        // Use entry API for atomic check-and-insert
        // This prevents race conditions where two threads could assign different IDs
        // to the same string
        let arc_str: Arc<str> = Arc::from(string);

        self.string_to_id
            .entry(arc_str.clone())
            .or_try_insert_with(|| {
                // Atomically reserve an ID first to prevent capacity check race
                // Use Acquire to ensure we see the latest count, Release to publish our reservation
                let id_value = self.next_id.fetch_add(1, Ordering::AcqRel);

                // Check if we exceeded capacity AFTER reserving ID
                if id_value >= self.max_capacity as u32 {
                    // Best effort: undo the reservation
                    self.next_id.fetch_sub(1, Ordering::AcqRel);

                    return Err(crate::utils::error::Error::Storage(
                        crate::utils::error::StorageError::CapacityExceeded {
                            resource: "string interner".to_string(),
                            current: id_value as usize,
                            limit: self.max_capacity,
                        },
                    ));
                }

                let id = InternedString(id_value);

                // Store the reverse mapping
                self.id_to_string.insert(id, arc_str.clone());

                Ok(id)
            })
            .map(|r| *r)
    }

    /// Intern a string without capacity checks.
    ///
    /// # Safety
    /// This method bypasses capacity limits. It should only be used in trusted
    /// internal contexts where the string is known to be valid and necessary,
    /// such as WAL recovery or deserialization of known-good data.
    ///
    /// Using this method with untrusted input could lead to unbounded memory growth.
    #[inline]
    #[allow(dead_code)] // Available for internal use (WAL recovery, etc.)
    pub(crate) fn intern_unchecked<S: AsRef<str>>(&self, string: S) -> InternedString {
        let string = string.as_ref();

        // Fast path: check if already interned
        if let Some(id) = self.get_id(string) {
            return id;
        }

        // Slow path: intern without capacity check
        let arc_str: Arc<str> = Arc::from(string);

        *self.string_to_id.entry(arc_str.clone()).or_insert_with(|| {
            let id_value = self.next_id.fetch_add(1, Ordering::AcqRel);
            let id = InternedString(id_value);
            self.id_to_string.insert(id, arc_str.clone());
            id
        })
    }

    /// Resolve an interned string ID back to the original string.
    ///
    /// Returns None if the ID is not valid (was never interned).
    ///
    /// # Performance Note
    ///
    /// This method clones the underlying `Arc<str>`, which involves atomic reference
    /// counting operations.
    ///
    /// - **For read-only access**: Use [`resolve_with`](Self::resolve_with) to avoid Arc cloning.
    /// - **When an owned Arc is needed**: Use this method (`resolve`).
    #[deprecated(
        since = "0.1.0",
        note = "Use resolve_with() for read-only access; use resolve() only when an owned Arc<str> is strictly required"
    )]
    pub fn resolve(&self, id: InternedString) -> Option<Arc<str>> {
        self.id_to_string
            .get(&id)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// Get the string as a &str without cloning the Arc.
    ///
    /// This is useful when you just need to read the string temporarily.
    ///
    /// **Note:** For read-only access, prefer [`resolve_with`](Self::resolve_with) to avoid Arc cloning overhead.
    /// This method still clones the Arc internally.
    pub fn get(&self, id: InternedString) -> Option<impl AsRef<str> + '_> {
        self.id_to_string.get(&id).map(|entry| {
            let arc: Arc<str> = Arc::clone(entry.value());
            arc
        })
    }

    /// Access the interned string via a callback without cloning the Arc.
    ///
    /// This is more efficient than `resolve()` or `get()` when you only need
    /// temporary read access to the string, as it avoids atomic reference
    /// counting operations.
    ///
    /// This is particularly useful for:
    /// - Display and logging operations
    /// - Serialization
    /// - String comparisons
    /// - Any read-only operation that doesn't need to own the string
    ///
    /// # Performance Example
    ///
    /// When serializing property keys, this avoids Arc clones:
    /// ```ignore
    /// // Instead of:
    /// let key_str = interner.resolve(key_id)?;
    /// serializer.serialize_str(key_str.as_ref())?; // Arc clone overhead
    ///
    /// // Use:
    /// interner.resolve_with(key_id, |s| serializer.serialize_str(s))?;
    /// ```
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::core::interning::StringInterner;
    ///
    /// let interner = StringInterner::new();
    /// let id = interner.intern("hello").unwrap();
    ///
    /// // Efficient: no Arc clone
    /// let len = interner.resolve_with(id, |s| s.len()).unwrap();
    /// assert_eq!(len, 5);
    ///
    /// // Can return any type from the callback
    /// let uppercase = interner.resolve_with(id, |s| s.to_uppercase()).unwrap();
    /// assert_eq!(uppercase, "HELLO");
    /// ```
    pub fn resolve_with<F, R>(&self, id: InternedString, f: F) -> Option<R>
    where
        F: FnOnce(&str) -> R,
    {
        self.id_to_string
            .get(&id)
            .map(|entry| f(entry.value().as_ref()))
    }

    /// Access the interned string via a callback (deprecated - use [`resolve_with`](Self::resolve_with)).
    #[deprecated(since = "0.1.0", note = "Use resolve_with() instead")]
    pub fn with_str<F, R>(&self, id: InternedString, f: F) -> Option<R>
    where
        F: FnOnce(&str) -> R,
    {
        self.resolve_with(id, f)
    }

    /// Check if a string has been interned.
    pub fn contains<S: AsRef<str>>(&self, string: S) -> bool {
        self.string_to_id.contains_key(string.as_ref())
    }

    /// Get the ID of a string if it has been interned.
    pub fn get_id<S: AsRef<str>>(&self, string: S) -> Option<InternedString> {
        self.string_to_id
            .get(string.as_ref())
            .map(|entry| *entry.value())
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
        self.next_id.store(0, Ordering::Release);
    }

    /// Get all interned strings in ID order.
    ///
    /// Returns a vector of strings sorted by their InternedString IDs.
    /// This is useful for persistence where we need to save and restore
    /// the interner state.
    pub fn get_all_strings(&self) -> Vec<String> {
        // Use next_id to determine size, as it's the authority on allocated IDs.
        // self.len() (from string_to_id map) might lag behind during concurrent inserts.
        // We use Acquire ordering to ensure we see updates from other threads that have
        // completed their insertions.
        let estimated_count = self.next_id.load(Ordering::Acquire) as usize;
        let mut strings = vec![String::new(); estimated_count];

        // Collect all (id, string) pairs
        for entry in self.id_to_string.iter() {
            let id = entry.key().as_u32() as usize;

            // If we encounter an ID that exceeds our initial estimate (due to race),
            // resize the vector to accommodate it. This ensures no data is lost
            // even if the interner is being modified concurrently.
            if id >= strings.len() {
                strings.resize(id + 1, String::new());
            }

            strings[id] = entry.value().to_string();
        }

        strings
    }

    /// Pre-intern common strings at startup to avoid initial allocation overhead.
    ///
    /// This method interns frequently used property keys and labels that are
    /// commonly accessed throughout the database's operational lifetime. By
    /// pre-interning these strings, we eliminate the "allocation + hash + insert"
    /// penalty on first access, providing predictable startup performance.
    ///
    /// The list of common strings is defined in [`COMMON_STRINGS`] and includes:
    /// - Property keys: "name", "id", "type", "label"
    /// - Timestamps: "created_at", "updated_at"
    /// - Temporal: "valid_from", "valid_to", "tx_from", "tx_to"
    ///
    /// This method is idempotent - calling it multiple times will not duplicate
    /// strings or change their IDs.
    ///
    /// # Performance Impact
    ///
    /// The performance impact is minimal and only affects the initial access cost
    /// for each string. Subsequent accesses remain efficient with O(1) lookups.
    ///
    /// # Example
    ///
    /// ```
    /// use aletheiadb::core::interning::StringInterner;
    ///
    /// let interner = StringInterner::new();
    /// interner.warm_common_strings();
    ///
    /// // These strings are now pre-interned (no allocation on first access)
    /// let id = interner.intern("name").unwrap();
    /// ```
    pub fn warm_common_strings(&self) {
        // Intern each common string using intern_unchecked since these are
        // known-good strings from a trusted internal source
        for s in COMMON_STRINGS {
            let _id = self.intern_unchecked(s);
        }
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
/// use aletheiadb::core::interning::GLOBAL_INTERNER;
///
/// let id1 = GLOBAL_INTERNER.intern("Person").unwrap();
/// let id2 = GLOBAL_INTERNER.intern("Person").unwrap();
/// assert_eq!(id1, id2); // Same string gets same ID
///
/// let string = GLOBAL_INTERNER.resolve(id1).unwrap();
/// assert_eq!(string.as_ref(), "Person");
/// ```
use std::sync::LazyLock;

/// Environment variable to configure the maximum number of interned strings.
///
/// Set `GALLIFREYDB_MAX_INTERNED_STRINGS` to override the default limit of 100,000.
/// This is useful for large knowledge graphs with many unique labels or property keys.
///
/// Example: `GALLIFREYDB_MAX_INTERNED_STRINGS=1000000`
pub const MAX_INTERNED_STRINGS_ENV: &str = "GALLIFREYDB_MAX_INTERNED_STRINGS";

/// Global string interner for sharing common strings across the database.
///
/// This static provides a single, thread-safe string interner that can be used
/// throughout the application to deduplicate common strings like labels and property keys.
///
/// The global interner is automatically warmed at initialization with common strings
/// such as "name", "id", "type", "label", "created_at", "updated_at", "valid_from",
/// "valid_to", "tx_from", and "tx_to" to provide predictable startup performance.
///
/// ## Configuration
///
/// The maximum capacity can be configured via the `GALLIFREYDB_MAX_INTERNED_STRINGS`
/// environment variable. If not set, defaults to 100,000.
pub static GLOBAL_INTERNER: LazyLock<StringInterner> = LazyLock::new(|| {
    let max_capacity = std::env::var(MAX_INTERNED_STRINGS_ENV)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_INTERNED_STRINGS);

    let interner = StringInterner::with_max_capacity(max_capacity);
    interner.warm_common_strings();
    interner
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_same_string() {
        let interner = StringInterner::new();

        let id1 = interner.intern("hello").unwrap();
        let id2 = interner.intern("hello").unwrap();

        assert_eq!(id1, id2, "Same string should get same ID");
    }

    #[test]
    fn test_intern_different_strings() {
        let interner = StringInterner::new();

        let id1 = interner.intern("hello").unwrap();
        let id2 = interner.intern("world").unwrap();

        assert_ne!(id1, id2, "Different strings should get different IDs");
    }

    #[test]
    #[allow(deprecated)]
    fn test_resolve() {
        let interner = StringInterner::new();

        let id = interner.intern("test").unwrap();
        let resolved = interner.resolve(id).expect("Should resolve");

        assert_eq!(resolved.as_ref(), "test");
    }

    #[test]
    #[allow(deprecated)]
    fn test_resolve_invalid_id() {
        let interner = StringInterner::new();

        let invalid_id = InternedString::from_raw(999);
        assert!(interner.resolve(invalid_id).is_none());
    }

    #[test]
    fn test_contains() {
        let interner = StringInterner::new();

        assert!(!interner.contains("test"));

        interner.intern("test").unwrap();

        assert!(interner.contains("test"));
        assert!(!interner.contains("other"));
    }

    #[test]
    fn test_get_id() {
        let interner = StringInterner::new();

        assert_eq!(interner.get_id("test"), None);

        let id = interner.intern("test").unwrap();

        assert_eq!(interner.get_id("test"), Some(id));
    }

    #[test]
    fn test_len() {
        let interner = StringInterner::new();

        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());

        interner.intern("a").unwrap();
        interner.intern("b").unwrap();
        interner.intern("a").unwrap(); // Duplicate, shouldn't increase count

        assert_eq!(interner.len(), 2);
        assert!(!interner.is_empty());
    }

    #[test]
    #[allow(deprecated)]
    fn test_clear() {
        let interner = StringInterner::new();

        let id = interner.intern("test").unwrap();
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
                let id1 = interner_clone.intern("concurrent").unwrap();
                let id2 = interner_clone.intern("test").unwrap();
                (id1, id2)
            });
            handles.push(handle);
        }

        // Collect all results
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

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
    #[allow(deprecated)]
    fn test_global_interner() {
        let id1 = GLOBAL_INTERNER.intern("global").unwrap();
        let id2 = GLOBAL_INTERNER.intern("global").unwrap();

        assert_eq!(id1, id2);

        let resolved = GLOBAL_INTERNER.resolve(id1).unwrap();
        assert_eq!(resolved.as_ref(), "global");
    }

    #[test]
    fn test_resolve_with_basic() {
        let interner = StringInterner::new();
        let id = interner.intern("hello").unwrap();

        // Test basic access
        let result = interner.resolve_with(id, |s| {
            assert_eq!(s, "hello");
            s.len()
        });

        assert_eq!(result, Some(5));
    }

    #[test]
    fn test_resolve_with_invalid_id() {
        let interner = StringInterner::new();
        let invalid_id = InternedString::from_raw(999);

        let result = interner.resolve_with(invalid_id, |s| s.len());
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_with_return_types() {
        let interner = StringInterner::new();
        let id = interner.intern("test string").unwrap();

        // Return usize
        let len = interner.resolve_with(id, |s| s.len()).unwrap();
        assert_eq!(len, 11);

        // Return String
        let uppercase = interner.resolve_with(id, |s| s.to_uppercase()).unwrap();
        assert_eq!(uppercase, "TEST STRING");

        // Return bool
        let contains = interner.resolve_with(id, |s| s.contains("test")).unwrap();
        assert!(contains);

        // Return Vec (must own the data since it outlives the callback)
        let words: Vec<String> = interner
            .resolve_with(id, |s| {
                s.split_whitespace().map(|w| w.to_string()).collect()
            })
            .unwrap();
        assert_eq!(words, vec!["test", "string"]);
    }

    #[test]
    #[allow(deprecated)]
    fn test_resolve_with_no_arc_clone() {
        let interner = StringInterner::new();
        let id = interner.intern("performance test").unwrap();

        // Get a baseline Arc to check the strong count.
        // We don't hardcode the count because DashMap's internal structure may vary.
        let s_arc = interner.resolve(id).unwrap();
        let baseline_count = Arc::strong_count(&s_arc);

        // This test verifies that resolve_with works without cloning by checking the refcount.
        let mut call_count = 0;
        let result = interner.resolve_with(id, |s| {
            call_count += 1;
            // The count should not increase during the callback.
            assert_eq!(Arc::strong_count(&s_arc), baseline_count);
            s.to_string()
        });

        // The count should remain unchanged after the call.
        assert_eq!(Arc::strong_count(&s_arc), baseline_count);
        assert_eq!(result, Some("performance test".to_string()));
        assert_eq!(call_count, 1);
    }

    #[test]
    fn test_resolve_with_concurrent() {
        use std::thread;

        let interner = Arc::new(StringInterner::new());
        let id = interner.intern("concurrent").unwrap();

        // Spawn 10 threads using a scope to ensure they all complete.
        let results: Vec<String> = thread::scope(|s| {
            let mut handles = Vec::new();
            for i in 0..10 {
                let interner_clone = Arc::clone(&interner);
                handles.push(s.spawn(move || {
                    interner_clone
                        .resolve_with(id, |s| {
                            assert_eq!(s, "concurrent");
                            format!("{}-{}", s, i)
                        })
                        .unwrap()
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        // Verify each thread got the correct result
        for (i, result) in results.iter().enumerate() {
            assert_eq!(result, &format!("concurrent-{}", i));
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_resolve_with_vs_resolve_equivalence() {
        let interner = StringInterner::new();
        let id = interner.intern("equivalence test").unwrap();

        // Both methods should give the same string content
        let via_resolve_with = interner.resolve_with(id, |s| s.to_string()).unwrap();
        let via_resolve = interner.resolve(id).unwrap();

        assert_eq!(via_resolve_with, via_resolve.as_ref());
    }

    #[test]
    fn test_resolve_with_empty_string() {
        let interner = StringInterner::new();
        let id = interner.intern("").unwrap();

        let len = interner.resolve_with(id, |s| s.len()).unwrap();
        assert_eq!(len, 0);

        let is_empty = interner.resolve_with(id, |s| s.is_empty()).unwrap();
        assert!(is_empty);
    }

    #[test]
    fn test_resolve_with_panic_safety() {
        let interner = StringInterner::new();
        let id = interner.intern("panic test").unwrap();

        // Verify the interner works before panic
        let before = interner.resolve_with(id, |s| s.to_string()).unwrap();
        assert_eq!(before, "panic test");

        // Cause a panic in the callback
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            interner.resolve_with(id, |_s| {
                panic!("intentional panic in callback");
            })
        }));

        // The panic should be caught
        assert!(result.is_err());

        // Verify the interner is still usable after the panic
        // The DashMap entry guard should have been properly dropped
        let after = interner.resolve_with(id, |s| s.to_string()).unwrap();
        assert_eq!(after, "panic test");

        // Verify we can still intern new strings
        let new_id = interner.intern("after panic").unwrap();
        let new_str = interner.resolve_with(new_id, |s| s.to_string()).unwrap();
        assert_eq!(new_str, "after panic");
    }

    // ========== New Concurrency & Error Handling Tests for Coverage ==========

    #[test]
    fn test_intern_capacity_exceeded_error() -> crate::utils::error::Result<()> {
        let interner = StringInterner::with_max_capacity(10);

        // Intern 10 strings - should all succeed
        for i in 0..10 {
            interner.intern(format!("string_{}", i))?;
        }

        assert_eq!(interner.len(), 10);

        // 11th string should fail with CapacityExceeded error
        let result = interner.intern("overflow");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                crate::utils::error::Error::Storage(
                    crate::utils::error::StorageError::CapacityExceeded { .. }
                )
            ),
            "Expected CapacityExceeded error, got: {:?}",
            err
        );

        // Should still have exactly 10 strings
        assert_eq!(interner.len(), 10);

        Ok(())
    }

    #[test]
    fn test_intern_concurrent_capacity_race() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let interner = Arc::new(StringInterner::with_max_capacity(100));
        let barrier = Arc::new(Barrier::new(10));
        let mut handles = vec![];

        // Spawn 10 threads, each trying to intern 20 unique strings (200 total attempts)
        for thread_id in 0..10 {
            let interner = Arc::clone(&interner);
            let barrier = Arc::clone(&barrier);

            let handle = thread::spawn(move || {
                barrier.wait(); // Synchronize all threads

                let mut success_count = 0;
                for i in 0..20 {
                    let string = format!("thread_{}_string_{}", thread_id, i);
                    if interner.intern(&string).is_ok() {
                        success_count += 1;
                    }
                }
                success_count
            });
            handles.push(handle);
        }

        // Collect results
        let total_successes: usize = handles
            .into_iter()
            .map(|h: std::thread::JoinHandle<usize>| h.join().unwrap())
            .sum();

        // Exactly 100 successful interns is guaranteed by atomic operations:
        // - fetch_add(1, Ordering::Relaxed) atomically reserves an ID
        // - If ID >= max_capacity, intern() returns error and undoes reservation
        // - This ensures exactly `max_capacity` successful interns, no more, no less
        assert_eq!(total_successes, 100);
        assert_eq!(interner.len(), 100);
    }

    #[test]
    fn test_concurrent_intern_same_string_deduplication() {
        use std::sync::Arc;
        use std::thread;

        let interner = Arc::new(StringInterner::new());
        let mut handles = vec![];

        // 100 threads all trying to intern the same string "concurrent"
        for _ in 0..100 {
            let interner = Arc::clone(&interner);
            let handle = thread::spawn(move || interner.intern("concurrent").unwrap());
            handles.push(handle);
        }

        // Collect all IDs
        let ids: Vec<_> = handles
            .into_iter()
            .map(|h: std::thread::JoinHandle<InternedString>| h.join().unwrap())
            .collect();

        // All IDs should be identical (deduplication)
        let first_id = ids[0];
        for id in &ids[1..] {
            assert_eq!(*id, first_id);
        }

        // Should only have 1 unique string
        assert_eq!(interner.len(), 1);
    }

    // ========== Tests for warm_common_strings() ==========

    #[test]
    fn test_warm_common_strings_basic() {
        let interner = StringInterner::new();

        // Before warming, interner should be empty
        assert_eq!(interner.len(), 0);

        // Warm common strings
        interner.warm_common_strings();

        // After warming, common strings should be interned
        assert!(!interner.is_empty());

        // Verify all common strings are present
        for s in COMMON_STRINGS {
            assert!(
                interner.contains(s),
                "Common string '{}' should be interned after warming",
                s
            );
        }

        // Should have exactly the number of common strings
        assert_eq!(interner.len(), COMMON_STRINGS.len());
    }

    #[test]
    fn test_warm_common_strings_idempotent() {
        let interner = StringInterner::new();

        // First warming
        interner.warm_common_strings();
        let len_after_first = interner.len();

        // Get IDs of some common strings
        let id_name_1 = interner.get_id("name").unwrap();
        let id_type_1 = interner.get_id("type").unwrap();

        // Second warming should not change anything
        interner.warm_common_strings();
        let len_after_second = interner.len();

        // Length should be the same
        assert_eq!(len_after_first, len_after_second);

        // IDs should be the same
        let id_name_2 = interner.get_id("name").unwrap();
        let id_type_2 = interner.get_id("type").unwrap();

        assert_eq!(id_name_1, id_name_2);
        assert_eq!(id_type_1, id_type_2);
    }

    #[test]
    fn test_warm_common_strings_no_allocation_on_subsequent_access() {
        let interner = StringInterner::new();

        // Warm common strings
        interner.warm_common_strings();

        // Subsequent intern calls should return existing IDs without allocation
        let id_before = interner.get_id("name").unwrap();
        let id_after = interner.intern("name").unwrap();

        assert_eq!(id_before, id_after);

        // Length shouldn't change
        let expected_len = interner.len();
        interner.intern("type").unwrap();
        interner.intern("label").unwrap();
        assert_eq!(interner.len(), expected_len);
    }

    #[test]
    fn test_warm_common_strings_sequential_ids() {
        let interner = StringInterner::new();

        // Warm common strings
        interner.warm_common_strings();

        // Common strings should get sequential IDs starting from 0
        let mut ids: Vec<_> = COMMON_STRINGS
            .iter()
            .map(|s| interner.get_id(s).unwrap().as_u32())
            .collect();

        ids.sort();

        // IDs should be 0, 1, 2, ..., n-1
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(*id, i as u32);
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_warm_common_strings_with_existing_data() {
        let interner = StringInterner::new();

        // Intern some strings before warming
        let existing_id = interner.intern("existing").unwrap();

        // Warm common strings
        interner.warm_common_strings();

        // Existing ID should still be valid
        assert_eq!(interner.resolve(existing_id).unwrap().as_ref(), "existing");

        // New common strings should also be present
        assert!(interner.contains("name"));
        assert!(interner.contains("type"));

        // Total should be 1 (existing) + 10 (common strings)
        assert_eq!(interner.len(), 11);
    }

    #[test]
    fn test_warm_common_strings_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let interner = Arc::new(StringInterner::new());

        // Spawn multiple threads that all call warm_common_strings
        let mut handles = vec![];
        for _ in 0..10 {
            let interner_clone = Arc::clone(&interner);
            let handle = thread::spawn(move || {
                interner_clone.warm_common_strings();
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Should still have exactly the common strings count (no duplicates)
        assert_eq!(interner.len(), COMMON_STRINGS.len());

        // All common strings should be present
        for s in COMMON_STRINGS {
            assert!(interner.contains(s));
        }
    }

    #[test]
    fn test_warm_common_strings_performance_benefit() {
        let interner = StringInterner::new();

        // Warm common strings
        interner.warm_common_strings();

        // This test verifies that warmed strings are already in the fast path
        // by checking they can be retrieved without error
        for s in COMMON_STRINGS {
            // get_id uses the fast path (no allocation)
            let id = interner.get_id(s);
            assert!(id.is_some(), "String '{}' should be pre-interned", s);

            // intern should also be fast (returns existing ID)
            let id_from_intern = interner.intern(s).unwrap();
            assert_eq!(id.unwrap(), id_from_intern);
        }
    }

    #[test]
    fn test_global_interner_automatically_warmed() {
        // GLOBAL_INTERNER should be automatically warmed at initialization
        // All common strings should already be present in the global interner
        for s in COMMON_STRINGS {
            assert!(
                GLOBAL_INTERNER.contains(s),
                "Global interner should have '{}' pre-warmed",
                s
            );
        }

        // The global interner should have at least the common strings
        // (might have more if other tests have used it)
        assert!(
            GLOBAL_INTERNER.len() >= COMMON_STRINGS.len(),
            "Global interner should have at least {} strings, has {}",
            COMMON_STRINGS.len(),
            GLOBAL_INTERNER.len()
        );
    }

    #[test]
    fn test_identity_hasher() {
        use std::hash::Hasher;
        let mut hasher = IdentityHasher::default();

        // Test write_u32 (primary path)
        hasher.write_u32(42);
        assert_eq!(hasher.finish(), 42);

        // Test write with 4 bytes (fallback success path)
        let bytes = 12345u32.to_le_bytes();
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 12345);

        // Test write with other length (fallback fail path)
        // This covers the else branch in IdentityHasher::write
        let bytes = [1u8, 2, 3];
        hasher.write(&bytes);
        assert_eq!(hasher.finish(), 3); // Should use len()
    }

    #[test]
    fn test_display_impl() {
        // Test successful resolution via GLOBAL_INTERNER
        let s = "display_test_string";
        let id = GLOBAL_INTERNER.intern(s).unwrap();
        assert_eq!(format!("{}", id), s);

        // Test fallback (ID not in GLOBAL_INTERNER)
        // We assume 1 billion is not used yet
        let raw_id = 1_000_000_000;
        let id = InternedString::from_raw(raw_id);
        assert_eq!(format!("{}", id), format!("Interned({})", raw_id));
    }
}

#[cfg(test)]
mod mutant_kill_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_with_max_capacity_completes_and_honors_limit_via_subprocess() {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let exe = std::env::current_exe().expect("failed to locate current test binary");
        let mut child = Command::new(exe)
            .args([
                "--ignored",
                "--exact",
                "core::interning::mutant_kill_tests::test_with_max_capacity_subprocess_helper",
            ])
            .spawn()
            .expect("failed to spawn subprocess for with_max_capacity test");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "subprocess helper failed for StringInterner::with_max_capacity"
                    );
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("StringInterner::with_max_capacity did not complete");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("failed while polling subprocess: {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn test_with_max_capacity_subprocess_helper() {
        let interner = StringInterner::with_max_capacity(3);
        assert_eq!(interner.max_capacity, 3);

        interner.intern("a").unwrap();
        interner.intern("b").unwrap();
        interner.intern("c").unwrap();
        assert!(interner.intern("d").is_err());
    }

    #[test]
    #[allow(deprecated)]
    fn test_with_str_returns_callback_result_and_none_for_invalid_id() {
        let interner = StringInterner::new();
        let id = interner.intern("alpha").unwrap();

        let len = interner.with_str(id, |s| s.len());
        assert_eq!(len, Some(5));

        let invalid = interner.with_str(InternedString::from_raw(999_999), |s| s.len());
        assert_eq!(invalid, None);
    }

    #[test]
    fn test_get_all_strings_returns_id_ordered_contents() {
        let interner = StringInterner::new();
        let first = interner.intern("first").unwrap();
        let second = interner.intern("second").unwrap();
        let third = interner.intern("third").unwrap();

        assert_eq!(first.as_u32(), 0);
        assert_eq!(second.as_u32(), 1);
        assert_eq!(third.as_u32(), 2);

        let all = interner.get_all_strings();
        assert_eq!(all.len(), interner.len());
        assert_eq!(
            all,
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ]
        );
    }
    #[test]
    fn test_get_all_strings_no_panic_under_concurrent_inserts() {
        let interner = Arc::new(StringInterner::with_max_capacity(200_000));

        for i in 0..128 {
            interner.intern(format!("seed_{i}")).unwrap();
        }

        let writer = {
            let interner = Arc::clone(&interner);
            std::thread::spawn(move || {
                for i in 0..20_000 {
                    let _ = interner.intern(format!("writer_{i}"));
                    if i % 256 == 0 {
                        std::thread::yield_now();
                    }
                }
            })
        };

        for _ in 0..10_000 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = interner.get_all_strings();
            }));
            assert!(
                result.is_ok(),
                "get_all_strings panicked under concurrent inserts"
            );
        }

        writer.join().unwrap();
    }
}
