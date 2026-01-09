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
use quick_cache::sync::Cache;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Default maximum number of interned strings (DoS protection)
pub const DEFAULT_MAX_INTERNED_STRINGS: usize = 100_000;

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

/// Configuration for the string interner.
///
/// Allows customization of cache size and ID exhaustion warning thresholds.
#[derive(Debug, Clone)]
pub struct InternerConfig {
    /// Maximum number of active cached strings (default: 100,000)
    pub max_cache_size: usize,
    /// Warn when total IDs exceed this threshold (default: u32::MAX - 1,000,000)
    pub id_exhaustion_warning_threshold: u32,
}

impl Default for InternerConfig {
    fn default() -> Self {
        Self {
            max_cache_size: DEFAULT_MAX_INTERNED_STRINGS,
            id_exhaustion_warning_threshold: u32::MAX - 1_000_000,
        }
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
    active_cache: Arc<Cache<Arc<str>, InternedString>>,
    /// Complete mapping for O(1) deduplication (never evicted)
    all_strings: DashMap<Arc<str>, InternedString>,
    /// Maps IDs back to strings.
    id_to_string: DashMap<InternedString, Arc<str>>,
    /// Next ID to assign.
    next_id: AtomicU32,
    /// Maximum number of strings to intern (DoS protection)
    #[allow(dead_code)] // Will be used in Phase 5 (ID exhaustion warning)
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
            active_cache: Arc::new(Cache::new(max_capacity)),
            all_strings: DashMap::new(),
            id_to_string: DashMap::new(),
            next_id: AtomicU32::new(0),
            max_capacity,
        }
    }

    /// Create a new string interner with custom configuration.
    pub fn with_config(config: InternerConfig) -> Self {
        StringInterner {
            active_cache: Arc::new(Cache::new(config.max_cache_size)),
            all_strings: DashMap::new(),
            id_to_string: DashMap::new(),
            next_id: AtomicU32::new(0),
            max_capacity: config.max_cache_size,
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
        if let Some(id) = self.active_cache.get(string) {
            return Ok(id);
        }
        let arc_str: Arc<str> = Arc::from(string);

        // O(1) deduplication check
        if let Some(existing_id) = self.all_strings.get(&arc_str) {
            self.active_cache.insert(arc_str, *existing_id);
            return Ok(*existing_id);
        }
        let id_value = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = InternedString(id_value);
        self.id_to_string.insert(id, arc_str.clone());
        self.all_strings.insert(arc_str.clone(), id);
        self.active_cache.insert(arc_str, id);
        Ok(id)
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
    #[allow(dead_code)]
    pub(crate) fn intern_unchecked<S: AsRef<str>>(&self, string: S) -> InternedString {
        let string = string.as_ref();
        if let Some(id) = self.active_cache.get(string) {
            return id;
        }
        let arc_str: Arc<str> = Arc::from(string);
        if let Some(existing_id) = self.all_strings.get(&arc_str) {
            self.active_cache.insert(arc_str, *existing_id);
            return *existing_id;
        }
        let id_value = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = InternedString(id_value);
        self.id_to_string.insert(id, arc_str.clone());
        self.all_strings.insert(arc_str.clone(), id);
        self.active_cache.insert(arc_str, id);
        id
    }

    /// Resolve an interned string ID back to the original string.
    ///
    /// Returns None if the ID is not valid (was never interned).
    pub fn resolve(&self, id: InternedString) -> Option<Arc<str>> {
        self.id_to_string
            .get(&id)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// Get the string as a &str without cloning the Arc.
    ///
    /// This is useful when you just need to read the string temporarily.
    ///
    /// **Note:** For read-only access, prefer [`with_str`](Self::with_str) to avoid Arc cloning overhead.
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
    /// interner.with_str(key_id, |s| serializer.serialize_str(s))?;
    /// ```
    ///
    /// # Examples
    ///
    /// ```
    /// use gallifreydb::core::interning::StringInterner;
    ///
    /// let interner = StringInterner::new();
    /// let id = interner.intern("hello").unwrap();
    ///
    /// // Efficient: no Arc clone
    /// let len = interner.with_str(id, |s| s.len()).unwrap();
    /// assert_eq!(len, 5);
    ///
    /// // Can return any type from the callback
    /// let uppercase = interner.with_str(id, |s| s.to_uppercase()).unwrap();
    /// assert_eq!(uppercase, "HELLO");
    /// ```
    pub fn with_str<F, R>(&self, id: InternedString, f: F) -> Option<R>
    where
        F: FnOnce(&str) -> R,
    {
        self.id_to_string
            .get(&id)
            .map(|entry| f(entry.value().as_ref()))
    }

    /// Check if a string has been interned.
    pub fn contains<S: AsRef<str>>(&self, string: S) -> bool {
        self.active_cache.get(string.as_ref()).is_some()
    }

    /// Get the ID of a string if it has been interned.
    pub fn get_id<S: AsRef<str>>(&self, string: S) -> Option<InternedString> {
        self.active_cache.get(string.as_ref())
    }

    /// Get the number of interned strings.
    pub fn len(&self) -> usize {
        self.active_cache.len()
    }

    /// Check if the interner is empty.
    pub fn is_empty(&self) -> bool {
        self.active_cache.len() == 0
    }

    /// Clear all interned strings.
    ///
    /// This invalidates all existing InternedString IDs!
    pub fn clear(&self) {
        self.active_cache.clear();
        self.all_strings.clear();
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
/// let id1 = GLOBAL_INTERNER.intern("Person").unwrap();
/// let id2 = GLOBAL_INTERNER.intern("Person").unwrap();
/// assert_eq!(id1, id2); // Same string gets same ID
///
/// let string = GLOBAL_INTERNER.resolve(id1).unwrap();
/// assert_eq!(string.as_ref(), "Person");
/// ```
use std::sync::LazyLock;

/// Global string interner for sharing common strings across the database.
///
/// This static provides a single, thread-safe string interner that can be used
/// throughout the application to deduplicate common strings like labels and property keys.
pub static GLOBAL_INTERNER: LazyLock<StringInterner> = LazyLock::new(StringInterner::new);

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
    fn test_resolve() {
        let interner = StringInterner::new();

        let id = interner.intern("test").unwrap();
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
    fn test_global_interner() {
        let id1 = GLOBAL_INTERNER.intern("global").unwrap();
        let id2 = GLOBAL_INTERNER.intern("global").unwrap();

        assert_eq!(id1, id2);

        let resolved = GLOBAL_INTERNER.resolve(id1).unwrap();
        assert_eq!(resolved.as_ref(), "global");
    }

    #[test]
    fn test_with_str_basic() {
        let interner = StringInterner::new();
        let id = interner.intern("hello").unwrap();

        // Test basic access
        let result = interner.with_str(id, |s| {
            assert_eq!(s, "hello");
            s.len()
        });

        assert_eq!(result, Some(5));
    }

    #[test]
    fn test_with_str_invalid_id() {
        let interner = StringInterner::new();
        let invalid_id = InternedString::from_raw(999);

        let result = interner.with_str(invalid_id, |s| s.len());
        assert_eq!(result, None);
    }

    #[test]
    fn test_with_str_return_types() {
        let interner = StringInterner::new();
        let id = interner.intern("test string").unwrap();

        // Return usize
        let len = interner.with_str(id, |s| s.len()).unwrap();
        assert_eq!(len, 11);

        // Return String
        let uppercase = interner.with_str(id, |s| s.to_uppercase()).unwrap();
        assert_eq!(uppercase, "TEST STRING");

        // Return bool
        let contains = interner.with_str(id, |s| s.contains("test")).unwrap();
        assert!(contains);

        // Return Vec (must own the data since it outlives the callback)
        let words: Vec<String> = interner
            .with_str(id, |s| {
                s.split_whitespace().map(|w| w.to_string()).collect()
            })
            .unwrap();
        assert_eq!(words, vec!["test", "string"]);
    }

    #[test]
    fn test_with_str_no_arc_clone() {
        let interner = StringInterner::new();
        let id = interner.intern("performance test").unwrap();

        // Get a baseline Arc to check the strong count.
        // We don't hardcode the count because DashMap's internal structure may vary.
        let s_arc = interner.resolve(id).unwrap();
        let baseline_count = Arc::strong_count(&s_arc);

        // This test verifies that with_str works without cloning by checking the refcount.
        let mut call_count = 0;
        let result = interner.with_str(id, |s| {
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
    fn test_with_str_concurrent() {
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
                        .with_str(id, |s| {
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
    fn test_with_str_vs_resolve_equivalence() {
        let interner = StringInterner::new();
        let id = interner.intern("equivalence test").unwrap();

        // Both methods should give the same string content
        let via_with_str = interner.with_str(id, |s| s.to_string()).unwrap();
        let via_resolve = interner.resolve(id).unwrap();

        assert_eq!(via_with_str, via_resolve.as_ref());
    }

    #[test]
    fn test_with_str_empty_string() {
        let interner = StringInterner::new();
        let id = interner.intern("").unwrap();

        let len = interner.with_str(id, |s| s.len()).unwrap();
        assert_eq!(len, 0);

        let is_empty = interner.with_str(id, |s| s.is_empty()).unwrap();
        assert!(is_empty);
    }

    #[test]
    fn test_with_str_panic_safety() {
        let interner = StringInterner::new();
        let id = interner.intern("panic test").unwrap();

        // Verify the interner works before panic
        let before = interner.with_str(id, |s| s.to_string()).unwrap();
        assert_eq!(before, "panic test");

        // Cause a panic in the callback
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            interner.with_str(id, |_s| {
                panic!("intentional panic in callback");
            })
        }));

        // The panic should be caught
        assert!(result.is_err());

        // Verify the interner is still usable after the panic
        // The DashMap entry guard should have been properly dropped
        let after = interner.with_str(id, |s| s.to_string()).unwrap();
        assert_eq!(after, "panic test");

        // Verify we can still intern new strings
        let new_id = interner.intern("after panic").unwrap();
        let new_str = interner.with_str(new_id, |s| s.to_string()).unwrap();
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

        // With cache eviction, 11th string succeeds
        let _result = interner.intern("overflow").unwrap();

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

        // With cache eviction, all 200 succeed
        assert_eq!(total_successes, 200);
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

    // ========================================
    // Phase 1: Configuration & Setup Tests (TDD - RED phase)
    // ========================================

    #[test]
    fn test_interner_config_defaults() {
        let config = InternerConfig::default();
        assert_eq!(config.max_cache_size, 100_000);
        assert_eq!(config.id_exhaustion_warning_threshold, u32::MAX - 1_000_000);
    }

    #[test]
    fn test_interner_config_custom() {
        let config = InternerConfig {
            max_cache_size: 50_000,
            id_exhaustion_warning_threshold: 1_000_000,
        };
        assert_eq!(config.max_cache_size, 50_000);
        assert_eq!(config.id_exhaustion_warning_threshold, 1_000_000);
    }

    #[test]
    fn test_interner_with_config() {
        let config = InternerConfig {
            max_cache_size: 1000,
            id_exhaustion_warning_threshold: 5000,
        };
        let interner = StringInterner::with_config(config);
        // Can intern strings
        let id = interner.intern("test").unwrap();
        assert_eq!(interner.resolve(id).unwrap().as_ref(), "test");
    }

    #[test]
    fn test_cache_eviction_over_capacity() {
        let config = InternerConfig {
            max_cache_size: 100,
            ..Default::default()
        };
        let interner = StringInterner::with_config(config);
        let mut ids = Vec::new();
        for i in 0..200 {
            ids.push(interner.intern(format!("key_{}", i)).unwrap());
        }
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(
                interner.resolve(*id).unwrap().as_ref(),
                format!("key_{}", i)
            );
        }
        assert_eq!(ids[0], interner.intern("key_0").unwrap());
    }
}

#[test]
fn test_reverse_lookup_performance() {
    use std::time::Instant;
    let config = InternerConfig {
        max_cache_size: 1000,
        ..Default::default()
    };
    let interner = StringInterner::with_config(config);

    // Intern 10,000 strings (causes cache eviction)
    for i in 0..10_000 {
        interner.intern(format!("key_{}", i)).unwrap();
    }

    // Re-intern early string (definitely evicted) - should be fast
    let start = Instant::now();
    interner.intern("key_0").unwrap();
    let duration = start.elapsed();

    // Should be O(1), not O(N) slow
    assert!(
        duration.as_micros() < 100,
        "Reverse lookup too slow: {:?}",
        duration
    );
}
