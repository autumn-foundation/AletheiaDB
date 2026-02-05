//! PropertyMap and PropertyMapBuilder implementation.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::constants::*;
use crate::core::property::value::{PropertyKey, PropertyValue};
use crate::utils::error::{Result, StorageError};

/// A map of property keys to values with copy-on-write semantics.
///
/// The underlying HashMap is wrapped in an Arc, making clones very cheap
/// (just incrementing a reference count). This enables efficient sharing
/// of unchanged properties across versions.
#[derive(Clone, PartialEq)]
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue>>,
    /// Cached serialized size in bytes.
    ///
    /// # Invariants
    ///
    /// This field must strictly equal the result of `serialized_size()` if calculated
    /// from scratch. It is calculated at creation time and maintained incrementally
    /// by `PropertyMapBuilder` to allow O(1) access for WAL reservation.
    ///
    /// # Copy-on-Write Safety
    ///
    /// `PropertyMap` implements copy-on-write semantics. This struct is immutable once
    /// created. Any modification (via `PropertyMapBuilder`) creates a *new* instance
    /// with a new `cached_size`.
    ///
    /// While `inner` is wrapped in an `Arc` for cheap cloning, `cached_size` is
    /// copied by value. This is safe because the underlying `HashMap` is never
    /// mutated in place through shared references. The only way to "modify" a map
    /// is to create a new one, which calculates its own fresh `cached_size`.
    cached_size: usize,
}

impl PropertyMap {
    /// Create a new empty property map.
    pub fn new() -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::new()),
            cached_size: 4, // 4 bytes for the count field (0)
        }
    }

    /// Create a property map with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::with_capacity(capacity)),
            cached_size: 4, // 4 bytes for the count field (0)
        }
    }

    /// Get a property value by key.
    ///
    /// The key is looked up in the interner for efficient comparison.
    /// Returns None if the key hasn't been interned (and thus cannot be in the map).
    #[inline]
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        let interned_key = GLOBAL_INTERNER.get_id(key)?;
        self.get_by_interned_key(&interned_key)
    }

    /// Get a property value by an already-interned key.
    ///
    /// This is more efficient than `get()` when you already have an InternedString.
    /// For internal use and performance-critical paths.
    #[inline]
    pub fn get_by_interned_key(&self, key: &PropertyKey) -> Option<&PropertyValue> {
        self.inner.get(key)
    }

    /// Check if a property exists.
    ///
    /// The key is looked up in the interner for efficient comparison.
    /// Returns false if the key hasn't been interned (and thus cannot be in the map).
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        let Some(interned_key) = GLOBAL_INTERNER.get_id(key) else {
            return false;
        };
        self.contains_interned_key(&interned_key)
    }

    /// Check if a property exists by an already-interned key.
    ///
    /// This is more efficient than `contains_key()` when you already have an InternedString.
    /// For internal use and performance-critical paths.
    #[inline]
    pub fn contains_interned_key(&self, key: &PropertyKey) -> bool {
        self.inner.contains_key(key)
    }

    /// Get the number of properties.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if the property map is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Iterate over all key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&PropertyKey, &PropertyValue)> {
        self.inner.iter()
    }

    /// Get all property keys.
    pub fn keys(&self) -> impl Iterator<Item = &PropertyKey> {
        self.inner.keys()
    }

    /// Get all property values.
    pub fn values(&self) -> impl Iterator<Item = &PropertyValue> {
        self.inner.values()
    }

    /// Create a builder for modifying this property map.
    ///
    /// This enables copy-on-write: if the Arc has multiple references,
    /// the HashMap will be cloned before modification.
    pub fn builder(self) -> PropertyMapBuilder {
        PropertyMapBuilder::from_map(self)
    }

    /// Check if this property map contains any vector properties (dense or sparse).
    ///
    /// This is used to optimize the transaction commit path by only triggering
    /// temporal vector index updates when vector data is actually present.
    ///
    /// Note: This only checks top-level properties. Nested vectors inside
    /// Array values are not currently detected (vectors-in-arrays are not
    /// a supported use case in the current implementation).
    #[inline]
    pub fn contains_vector(&self) -> bool {
        self.inner
            .values()
            .any(|v| matches!(v, PropertyValue::Vector(_) | PropertyValue::SparseVector(_)))
    }

    // ========================================================================
    // Serialization Methods
    // ========================================================================

    /// Serialize this PropertyMap to bytes.
    ///
    /// # Binary Format
    /// ```text
    /// [count:4][key1_len:4][key1_bytes:key1_len][value1_bytes:...]...
    /// ```
    ///
    /// - Count: u32 little-endian, number of key-value pairs
    /// - For each key-value pair:
    ///   - Key length: u32 little-endian
    ///   - Key bytes: UTF-8 encoded string
    ///   - Value: Serialized PropertyValue (includes type tag)
    ///
    /// Note: HashMap ordering is not guaranteed, so serialization order
    /// may vary. This is acceptable for correctness but may affect
    /// byte-for-byte reproducibility.
    ///
    /// # Errors
    ///
    /// Returns an error if any PropertyKey cannot be resolved from the interner.
    /// This should never happen in practice as all keys are created via interning.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let mut buffer = Vec::with_capacity(self.cached_size);
        self.serialize_into(&mut buffer)?;
        Ok(buffer)
    }

    /// Serialize this PropertyMap into an existing buffer.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::InconsistentState` if any PropertyKey cannot be
    /// resolved from the interner, indicating data corruption.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) -> Result<()> {
        // Reserve space for the entire map to avoid reallocations
        buffer.reserve(self.cached_size);

        buffer.extend_from_slice(&(self.inner.len() as u32).to_le_bytes());
        for (key, value) in self.inner.iter() {
            // Serialize key: resolve InternedString to actual string
            // Use with_str to avoid Arc cloning overhead
            GLOBAL_INTERNER
                .resolve_with(*key, |key_str| {
                    let key_bytes = key_str.as_bytes();
                    buffer.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
                    buffer.extend_from_slice(key_bytes);
                })
                .ok_or_else(|| {
                    crate::utils::error::Error::Storage(StorageError::InconsistentState {
                        reason: format!(
                            "PropertyKey {} not found in interner - data corruption detected",
                            key.as_u32()
                        ),
                    })
                })?;

            // Serialize value
            value.serialize_into(buffer)?;
        }
        Ok(())
    }

    /// Deserialize a PropertyMap from bytes.
    ///
    /// Returns the deserialized map and the number of bytes consumed.
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 4 {
            return Err(StorageError::CorruptedData(
                "Buffer too short for PropertyMap count".to_string(),
            )
            .into());
        }

        let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;

        // Prevent DoS via memory exhaustion from malicious input
        if count > MAX_PROPERTY_MAP_CAPACITY {
            return Err(StorageError::CorruptedData(format!(
                "PropertyMap count {} exceeds maximum allowed {}",
                count, MAX_PROPERTY_MAP_CAPACITY
            ))
            .into());
        }

        let mut offset = 4;

        // Prevent DoS via pre-allocation amplification:
        // Ensure we have at least 5 bytes per entry (minimum size)
        // Key length (4) + Key data (0) + Value tag (1) = 5 bytes
        // Use checked arithmetic to prevent overflow in count * 5
        let min_required_bytes = count.saturating_mul(5);
        if bytes.len().saturating_sub(offset) < min_required_bytes {
            return Err(StorageError::CorruptedData(format!(
                "Insufficient buffer size for PropertyMap entries: need {} bytes, have {}",
                min_required_bytes,
                bytes.len().saturating_sub(offset)
            ))
            .into());
        }

        let mut map = HashMap::with_capacity(count);
        // Track the actual logical size of the map to validate against consumed bytes
        let mut calculated_size: usize = 4;

        for _ in 0..count {
            // Read key length
            if bytes.len() < offset + 4 {
                return Err(StorageError::CorruptedData(
                    "Buffer too short for property key length".to_string(),
                )
                .into());
            }
            // SAFETY: Length check above guarantees 4 bytes available
            let key_len =
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;

            // Read key
            if bytes.len() < offset + key_len {
                return Err(StorageError::CorruptedData(
                    "Buffer too short for property key data".to_string(),
                )
                .into());
            }
            let key_str = std::str::from_utf8(&bytes[offset..offset + key_len]).map_err(|e| {
                StorageError::CorruptedData(format!("Invalid UTF-8 in property key: {}", e))
            })?;
            // Intern the key for efficient storage and comparison
            let key = GLOBAL_INTERNER.intern(key_str)?;
            offset += key_len;

            // Read value
            let (value, consumed) = PropertyValue::deserialize(&bytes[offset..])?;

            // Validate size consistency
            let key_size = 4 + key_len;
            calculated_size = calculated_size
                .saturating_add(key_size)
                .saturating_add(value.serialized_size()?);

            offset += consumed;

            if map.insert(key, value).is_some() {
                // If we encounter a duplicate key, the map's logical size shrinks (replacement),
                // but the input stream 'offset' keeps growing. This mismatch indicates
                // a non-canonical or corrupted stream (standard serialization implies unique keys).
                //
                // We enforce strict validation: offset (consumed bytes) must match
                // the logical size of the constructed map.
                return Err(StorageError::CorruptedData(format!(
                    "Duplicate property key found during deserialization: '{}'. \
                     This indicates corrupted data or invalid serialization format.",
                    key_str
                ))
                .into());
            }
        }

        // Final validation: The bytes consumed must match the logical size of the map.
        // If they differ, it implies hidden data, duplicates (caught above), or
        // inconsistent size calculations.
        if offset != calculated_size {
            return Err(StorageError::CorruptedData(format!(
                "PropertyMap deserialization size mismatch: consumed {} bytes but logical size is {}. \
                 Data corruption suspected.",
                offset, calculated_size
            ))
            .into());
        }

        Ok((
            PropertyMap {
                inner: Arc::new(map),
                cached_size: calculated_size,
            },
            offset,
        ))
    }

    /// Estimate the heap memory usage of this property map in bytes.
    ///
    /// This provides a rough estimate of heap allocations, useful for memory
    /// accounting in tiered storage migration decisions. The estimate includes:
    ///
    /// - HashMap internal storage overhead
    /// - PropertyKey storage (interned, so minimal)
    /// - PropertyValue heap allocations (strings, vectors, etc.)
    ///
    /// Note: Due to Arc sharing, actual memory usage may be lower if this
    /// PropertyMap shares its underlying data with other instances.
    pub fn estimated_heap_size(&self) -> usize {
        // HashMap overhead: capacity * (key_size + value_size + ~8 bytes overhead per entry)
        let mut size = self.inner.capacity()
            * (std::mem::size_of::<PropertyKey>() + std::mem::size_of::<PropertyValue>() + 8);

        // Add heap sizes of individual values
        for value in self.inner.values() {
            size += value.estimated_heap_size();
        }

        size
    }

    /// Calculate the number of bytes required for serialization.
    ///
    /// This returns a cached value calculated during map construction, providing
    /// O(1) access. This is critical for WAL performance where we need to
    /// pre-allocate buffers.
    #[inline(always)]
    pub fn serialized_size(&self) -> usize {
        self.cached_size
    }
}

impl fmt::Debug for PropertyMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        // Collect entries and pre-resolve keys to sort them for deterministic output
        // We map to (resolved_str, raw_id, value)
        let mut entries: Vec<_> = self
            .inner
            .iter()
            .map(|(key, value)| {
                let resolved = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string());
                (resolved, *key, value)
            })
            .collect();

        // Sort by resolved string if available, otherwise by ID
        // Resolved keys always come before unresolved ones for consistency
        entries.sort_by(|(s1, k1, _), (s2, k2, _)| match (s1, s2) {
            (Some(a), Some(b)) => a.cmp(b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => k1.cmp(k2),
        });

        for (resolved, key, value) in entries {
            if let Some(key_str) = resolved {
                map.entry(&key_str, value);
            } else {
                map.entry(&key, value);
            }
        }
        map.finish()
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(PropertyKey, PropertyValue)> for PropertyMap {
    fn from_iter<I: IntoIterator<Item = (PropertyKey, PropertyValue)>>(iter: I) -> Self {
        let mut map = HashMap::new();
        let mut size: usize = 4; // Count field

        for (key, value) in iter {
            // Need key size for serialization
            let key_len = GLOBAL_INTERNER
                .resolve_with(key, |s| s.len())
                .unwrap_or(256);
            let key_size = 4 + key_len;
            let val_size = value
                .serialized_size()
                .expect("Recursion depth limit exceeded in FromIterator");

            size = size.saturating_add(key_size).saturating_add(val_size);

            if let Some(old_val) = map.insert(key, value) {
                // If replaced, subtract the size of the old entry (key + value)
                // Key size is the same since it's the same key ID
                size = size.saturating_sub(key_size).saturating_sub(
                    old_val
                        .serialized_size()
                        .expect("Recursion depth limit exceeded in FromIterator"),
                );
            }
        }

        PropertyMap {
            inner: Arc::new(map),
            cached_size: size,
        }
    }
}

/// Builder for creating or modifying property maps with copy-on-write semantics.
pub struct PropertyMapBuilder {
    map: HashMap<PropertyKey, PropertyValue>,
    current_size: usize,
}

impl PropertyMapBuilder {
    /// Create a new builder with an empty map.
    pub fn new() -> Self {
        PropertyMapBuilder {
            map: HashMap::new(),
            current_size: 4, // Count field
        }
    }

    /// Create a builder from an existing PropertyMap.
    ///
    /// This will clone the underlying HashMap if the Arc has multiple references,
    /// implementing copy-on-write semantics.
    pub fn from_map(prop_map: PropertyMap) -> Self {
        let current_size = prop_map.cached_size;
        let map = Arc::try_unwrap(prop_map.inner).unwrap_or_else(|arc| (*arc).clone());
        PropertyMapBuilder { map, current_size }
    }

    /// Insert a property.
    ///
    /// The key is automatically interned. If interning fails (capacity exceeded),
    /// returns self unchanged.
    ///
    /// Panics if recursion depth limit is exceeded.
    pub fn insert<V: Into<PropertyValue>>(self, key: &str, value: V) -> Self {
        self.try_insert(key, value)
            .expect("Property insertion failed (recursion depth limit exceeded)")
    }

    /// Insert a property (fallible).
    pub fn try_insert<V: Into<PropertyValue>>(mut self, key: &str, value: V) -> Result<Self> {
        let Ok(interned_key) = GLOBAL_INTERNER.intern(key) else {
            return Ok(self);
        };
        let val = value.into();
        let val_size = val.serialized_size()?;

        if let Some(old_val) = self.map.insert(interned_key, val) {
            // Replaced existing entry
            // Key size is unchanged (same key ID means same string)
            self.current_size = self
                .current_size
                .saturating_sub(old_val.serialized_size()?)
                .saturating_add(val_size);
        } else {
            // New entry
            let key_size = 4 + key.len(); // Length prefix (4) + string bytes
            self.current_size = self
                .current_size
                .saturating_add(key_size)
                .saturating_add(val_size);
        }
        Ok(self)
    }

    /// Insert a property with an already-interned key.
    ///
    /// Panics if recursion depth limit is exceeded.
    pub fn insert_by_key(self, key: PropertyKey, value: PropertyValue) -> Self {
        self.try_insert_by_key(key, value)
            .expect("Property insertion failed (recursion depth limit exceeded)")
    }

    /// Insert a property with an already-interned key (fallible).
    pub fn try_insert_by_key(mut self, key: PropertyKey, value: PropertyValue) -> Result<Self> {
        let val_size = value.serialized_size()?;

        if let Some(old_val) = self.map.insert(key, value) {
            // Replaced existing entry - key size constant
            self.current_size = self
                .current_size
                .saturating_sub(old_val.serialized_size()?)
                .saturating_add(val_size);
        } else {
            // New entry - need key size!
            // We must look up the string length since we only have the ID.
            // This is a tradeoff: we pay lookup cost for new keys, but
            // avoid it for updates and for subsequent serialization size checks.
            let key_len = GLOBAL_INTERNER
                .resolve_with(key, |s| s.len())
                .unwrap_or_else(|| {
                    // This should be unreachable if the PropertyKey is valid (which it should be).
                    // In debug builds, we panic to catch this state corruption.
                    // In release, we fallback to a safe estimate (256 bytes) to avoid crashing.
                    debug_assert!(false, "PropertyKey {} missing from interner", key.as_u32());
                    256
                });
            let key_size = 4 + key_len;
            self.current_size = self
                .current_size
                .saturating_add(key_size)
                .saturating_add(val_size);
        }
        Ok(self)
    }

    /// Insert a vector property (convenience method for embeddings).
    ///
    /// This is a convenience wrapper around `insert()` for vector properties,
    /// commonly used for storing embeddings in nodes and edges.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gallifreydb::core::property::PropertyMapBuilder;
    ///
    /// let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
    /// let props = PropertyMapBuilder::new()
    ///     .insert("name", "Document")
    ///     .insert_vector("embedding", &embedding)
    ///     .build();
    ///
    /// assert_eq!(
    ///     props.get("embedding").and_then(|v| v.as_vector()),
    ///     Some(&embedding[..])
    /// );
    /// ```
    pub fn insert_vector(self, key: &str, vector: &[f32]) -> Self {
        self.insert(key, PropertyValue::vector(vector))
    }

    /// Insert a vector property (fallible).
    pub fn try_insert_vector(self, key: &str, vector: &[f32]) -> Result<Self> {
        self.try_insert(key, PropertyValue::try_vector(vector)?)
    }

    /// Remove a property.
    ///
    /// The key is automatically interned before removal.
    /// If interning fails (capacity exceeded), returns self unchanged.
    ///
    /// Panics if serialization size calculation fails.
    pub fn remove(self, key: &str) -> Self {
        self.try_remove(key).expect("Property removal failed")
    }

    /// Remove a property (fallible).
    pub fn try_remove(self, key: &str) -> Result<Self> {
        let Some(interned_key) = GLOBAL_INTERNER.get_id(key) else {
            return Ok(self);
        };
        self.try_remove_by_key(&interned_key)
    }

    /// Remove a property by an already-interned key.
    ///
    /// Panics if serialization size calculation fails.
    pub fn remove_by_key(self, key: &PropertyKey) -> Self {
        self.try_remove_by_key(key)
            .expect("Property removal failed")
    }

    /// Remove a property by an already-interned key (fallible).
    pub fn try_remove_by_key(mut self, key: &PropertyKey) -> Result<Self> {
        let old_val = self.map.remove(key);
        if let Some(old_val) = old_val {
            let key_len = GLOBAL_INTERNER
                .resolve_with(*key, |s| s.len())
                .unwrap_or(256);
            let key_size = 4 + key_len;
            self.current_size = self
                .current_size
                .saturating_sub(key_size)
                .saturating_sub(old_val.serialized_size()?);
        }
        Ok(self)
    }

    /// Build the final PropertyMap.
    pub fn build(self) -> PropertyMap {
        PropertyMap {
            inner: Arc::new(self.map),
            cached_size: self.current_size,
        }
    }
}

impl Default for PropertyMapBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Macro for creating property maps with a convenient syntax.
///
/// # Examples
///
/// ```ignore
/// let props = properties! {
///     "name" => "Alice",
///     "age" => 30,
///     "active" => true,
/// };
/// ```
#[macro_export]
macro_rules! properties {
    ($($key:expr => $value:expr),* $(,)?) => {
        {
            let mut builder = $crate::core::property::PropertyMapBuilder::new();
            $(
                builder = builder.insert($key, $value);
            )*
            builder.build()
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InternedString;

    #[test]
    fn test_property_map_creation() {
        let map = PropertyMap::new();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);

        let map = PropertyMap::with_capacity(10);
        assert!(map.is_empty());
    }

    #[test]
    fn test_property_map_builder() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .build();

        assert_eq!(map.len(), 3);
        assert_eq!(map.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(map.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(map.get("active").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn test_property_map_copy_on_write() {
        let map1 = PropertyMapBuilder::new().insert("key", "value1").build();

        // Clone is cheap (just Arc increment)
        let map2 = map1.clone();
        assert_eq!(map1, map2);

        // Modify map2 (should not affect map1 due to copy-on-write)
        let map2 = map2.builder().insert("key", "value2").build();

        assert_ne!(map1, map2);
        assert_eq!(map1.get("key").and_then(|v| v.as_str()), Some("value1"));
        assert_eq!(map2.get("key").and_then(|v| v.as_str()), Some("value2"));
    }

    #[test]
    fn test_property_map_iteration() {
        let map = PropertyMapBuilder::new()
            .insert("a", 1i64)
            .insert("b", 2i64)
            .insert("c", 3i64)
            .build();

        let keys: Vec<_> = map.keys().cloned().collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&GLOBAL_INTERNER.intern("a").unwrap()));
        assert!(keys.contains(&GLOBAL_INTERNER.intern("b").unwrap()));
        assert!(keys.contains(&GLOBAL_INTERNER.intern("c").unwrap()));

        let values: Vec<_> = map.values().cloned().collect();
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn test_properties_macro() {
        let map = properties! {
            "name" => "Bob",
            "age" => 25,
            "score" => 98.5,
        };

        assert_eq!(map.len(), 3);
        assert_eq!(map.get("name").and_then(|v| v.as_str()), Some("Bob"));
        assert_eq!(map.get("age").and_then(|v| v.as_int()), Some(25));
        assert_eq!(map.get("score").and_then(|v| v.as_float()), Some(98.5));
    }

    #[test]
    fn test_serialize_property_map_empty() {
        let map = PropertyMap::new();
        let bytes = map.serialize().expect("Serialization should succeed");

        // Empty map: just count (4 bytes) = [0, 0, 0, 0]
        assert_eq!(bytes, vec![0, 0, 0, 0]);

        let (deserialized, consumed) = PropertyMap::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, map);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn test_serialize_property_map_round_trip() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .insert("score", 98.5)
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        // Check all values match
        assert_eq!(deserialized.len(), 4);
        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(deserialized.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(
            deserialized.get("active").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            deserialized.get("score").and_then(|v| v.as_float()),
            Some(98.5)
        );
    }

    #[test]
    fn test_serialize_property_map_with_vector() {
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let map = PropertyMapBuilder::new()
            .insert("name", "node1")
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("node1")
        );
        assert_eq!(
            deserialized.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_serialize_property_map_with_nested_array() {
        let map = PropertyMapBuilder::new()
            .insert(
                "tags",
                PropertyValue::array(vec![
                    PropertyValue::string("rust"),
                    PropertyValue::string("database"),
                ]),
            )
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        let tags = deserialized.get("tags").and_then(|v| v.as_array()).unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].as_str(), Some("rust"));
        assert_eq!(tags[1].as_str(), Some("database"));
    }

    #[test]
    fn test_property_map_deserialize_errors() {
        // Empty buffer
        let result = PropertyMap::deserialize(&[]);
        assert!(result.is_err());

        // Truncated count
        let result = PropertyMap::deserialize(&[1, 2]);
        assert!(result.is_err());

        // Says 1 key-value but no data
        let result = PropertyMap::deserialize(&[1, 0, 0, 0]);
        assert!(result.is_err());
    }

    // ========================================================================
    // PropertyKey Interning Tests (Issue #16)
    // ========================================================================

    #[test]
    fn test_property_key_interning_serialization_round_trip() {
        // Create a property map with interned keys
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .build();

        // Serialize
        let bytes = map.serialize().expect("Serialization should succeed");

        // Deserialize
        let (deserialized, _) =
            PropertyMap::deserialize(&bytes).expect("Deserialization should succeed");

        // Verify all values match
        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(deserialized.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(
            deserialized.get("active").and_then(|v| v.as_bool()),
            Some(true)
        );

        // Verify keys are interned (same ID as original)
        let original_keys: Vec<_> = map.keys().cloned().collect();
        let deserialized_keys: Vec<_> = deserialized.keys().cloned().collect();

        assert_eq!(original_keys.len(), deserialized_keys.len());
        for key in &original_keys {
            assert!(
                deserialized_keys.contains(key),
                "Key should be interned with same ID"
            );
        }
    }

    #[test]
    fn test_property_key_memory_efficiency() {
        use std::mem::size_of;

        // Verify InternedString is indeed smaller than String
        assert_eq!(
            size_of::<InternedString>(),
            4,
            "InternedString should be 4 bytes"
        );
        assert_eq!(size_of::<String>(), 24, "String should be 24 bytes");

        // Create multiple maps with the same keys
        let maps: Vec<_> = (0..100)
            .map(|i| {
                PropertyMapBuilder::new()
                    .insert("test_mem_name", format!("Person{}", i))
                    .insert("test_mem_age", i as i64)
                    .insert("test_mem_id", i as i64)
                    .build()
            })
            .collect();

        // Verify all maps use the same interned key IDs (order may vary)
        use std::collections::HashSet;
        let first_keys: HashSet<_> = maps[0].keys().cloned().collect();
        for map in &maps[1..] {
            let map_keys: HashSet<_> = map.keys().cloned().collect();
            assert_eq!(
                first_keys, map_keys,
                "All maps should share the same interned key IDs"
            );
        }

        // Verify we have exactly 3 unique keys across all maps
        assert_eq!(first_keys.len(), 3, "Should have exactly 3 unique keys");

        // Verify the specific keys exist and can be resolved
        let expected_keys = ["test_mem_name", "test_mem_age", "test_mem_id"];
        for key_str in &expected_keys {
            let exists = first_keys.iter().any(|key| {
                GLOBAL_INTERNER
                    .resolve_with(*key, |s| s == *key_str)
                    .unwrap_or(false)
            });
            assert!(
                exists,
                "Key '{}' should exist in the property maps",
                key_str
            );
        }
    }

    #[test]
    fn test_invalid_interned_string_serialization() {
        // Create an InternedString with a raw ID that doesn't exist in the interner
        let invalid_key = InternedString::from_raw(999999);

        // Create a property map and manually insert with invalid key
        let mut inner_map = HashMap::new();
        inner_map.insert(invalid_key, PropertyValue::Int(42));
        let map = PropertyMap {
            inner: Arc::new(inner_map),
            cached_size: 4, // Ignored
        };

        // Serialization should return an error, not panic
        let result = map.serialize();
        assert!(
            result.is_err(),
            "Serialization should fail for invalid InternedString"
        );

        match result {
            Err(crate::utils::error::Error::Storage(StorageError::InconsistentState {
                reason,
            })) => {
                assert!(
                    reason.contains("not found in interner"),
                    "Error message should indicate missing key in interner"
                );
            }
            _ => panic!("Expected StorageError::InconsistentState"),
        }
    }

    #[test]
    fn test_serialize_with_invalid_key() {
        // This explicitly verifies the error path when the interner is missing a key
        // Uses the same logic as test_invalid_interned_string_serialization but
        // ensures the new `ok_or_else` path is hit correctly.

        let invalid_key = InternedString::from_raw(888888);
        let mut inner_map = HashMap::new();
        inner_map.insert(invalid_key, PropertyValue::Bool(true));
        let map = PropertyMap {
            inner: Arc::new(inner_map),
            cached_size: 4, // Ignored
        };

        let mut buffer = Vec::new();
        let result = map.serialize_into(&mut buffer);

        assert!(result.is_err(), "Should return error for missing key");
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::InconsistentState {
                reason,
            })) => {
                assert!(
                    reason.contains("888888"),
                    "Error message should contain the invalid key ID"
                );
            }
            _ => panic!("Expected InconsistentState error"),
        }
    }

    #[test]
    fn test_concurrent_property_key_access() {
        use std::sync::Arc;
        use std::thread;

        // Create a property map
        let map = Arc::new(
            PropertyMapBuilder::new()
                .insert("shared_key", "value")
                .insert("count", 0i64)
                .build(),
        );

        // Spawn multiple threads accessing the same property keys
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let map_clone = Arc::clone(&map);
                thread::spawn(move || {
                    // Each thread accesses properties multiple times
                    for _ in 0..100 {
                        assert_eq!(
                            map_clone.get("shared_key").and_then(|v| v.as_str()),
                            Some("value")
                        );
                        assert!(map_clone.contains_key("count"));
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread should complete successfully");
        }
    }

    #[test]
    fn test_property_key_get_efficiency() {
        // Pre-populate interner with a key
        let interned_key = GLOBAL_INTERNER.intern("test_key").unwrap();

        let map = PropertyMapBuilder::new()
            .insert("test_key", "value")
            .build();

        // get() should auto-intern the key
        let value1 = map.get("test_key");
        assert_eq!(value1.and_then(|v| v.as_str()), Some("value"));

        // get_by_interned_key() should be more efficient for repeated lookups
        let value2 = map.get_by_interned_key(&interned_key);
        assert_eq!(value2.and_then(|v| v.as_str()), Some("value"));

        // Both methods should return the same result
        assert_eq!(value1, value2);
    }

    #[test]
    fn test_property_map_estimated_heap_size_empty() {
        let map = PropertyMap::new();
        // Empty map should have zero heap overhead
        let size = map.estimated_heap_size();
        assert_eq!(size, 0, "Empty map heap size should be zero");
    }

    #[test]
    fn test_property_map_estimated_heap_size_with_values() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice") // string with 5 chars
            .insert("age", 30i64) // primitive, no heap
            .insert("active", true) // primitive, no heap
            .build();

        let size = map.estimated_heap_size();
        // Should include at least the string length plus HashMap overhead
        assert!(size >= 5, "Map with string should include string heap size");
    }

    #[test]
    fn test_property_map_estimated_heap_size_with_vector() {
        let embedding = vec![0.1f32; 384];
        let map = PropertyMapBuilder::new()
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        let size = map.estimated_heap_size();
        // Should include vector heap size: 384 * 4 = 1536 bytes
        assert!(
            size >= 384 * std::mem::size_of::<f32>(),
            "Map with vector should include vector heap size"
        );
    }

    #[test]
    fn test_property_map_serialized_size() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build();

        let predicted = map.serialized_size();
        let actual = map.serialize().unwrap().len();
        assert_eq!(predicted, actual);
    }

    #[test]
    fn test_cached_size_tracking() {
        let builder = PropertyMapBuilder::new();

        // Initial size (count: 4)
        let map = builder.build();
        assert_eq!(map.serialized_size(), 4);

        // Insert
        let builder = map.builder().insert("key1", 100i64); // key1 (4+4) + 100 (9) = 17 + 4 = 21
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());

        // Another insert
        let builder = map.builder().insert("key2", "hello"); // key2 (4+4) + "hello" (1+4+5) = 8 + 10 = 18. Total 39.
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());

        // Update (replace value)
        let builder = map.builder().insert("key1", 200i64); // Same size
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());

        // Remove
        let builder = map.builder().remove("key2");
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());
    }

    #[test]
    fn test_cached_size_invariant() {
        // Property-based test: size should always match actual serialization
        let map = PropertyMapBuilder::new()
            .insert("a", 1)
            .insert("b", "test")
            .remove("a")
            .insert("c", vec![1.0f32, 2.0])
            .build();

        let serialized = map.serialize().unwrap();
        assert_eq!(map.serialized_size(), serialized.len());
        assert_eq!(map.cached_size, serialized.len());
    }

    #[test]
    fn test_from_iter_duplicate_keys() {
        // Test that FromIterator handles duplicate keys correctly with size tracking
        let items = vec![
            (
                GLOBAL_INTERNER.intern("key").unwrap(),
                PropertyValue::Int(1),
            ),
            (
                GLOBAL_INTERNER.intern("key").unwrap(),
                PropertyValue::Int(2),
            ), // duplicate!
        ];
        let map: PropertyMap = items.into_iter().collect();

        // Logical result: {"key": 2}
        // Size: 4 (count) + 4 (len) + 3 ("key") + 9 (Int) = 20

        let serialized = map.serialize().unwrap();
        assert_eq!(map.cached_size, serialized.len());
        assert_eq!(map.len(), 1); // Should only have one entry
        assert_eq!(map.get("key").and_then(|v| v.as_int()), Some(2));
    }

    #[test]
    fn test_deserialize_duplicate_keys_errors() {
        // Construct a buffer with duplicate keys manually
        // [count: 2][key: "a"][val: 1][key: "a"][val: 2]
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&2u32.to_le_bytes()); // Count: 2

        // Entry 1: "a" -> 1
        let key = "a";
        buffer.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buffer.extend_from_slice(key.as_bytes());
        PropertyValue::Int(1).serialize_into(&mut buffer).unwrap();

        // Entry 2: "a" -> 2 (Duplicate!)
        buffer.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buffer.extend_from_slice(key.as_bytes());
        PropertyValue::Int(2).serialize_into(&mut buffer).unwrap();

        // Deserialization should fail
        let result = PropertyMap::deserialize(&buffer);
        assert!(result.is_err());
        match result {
            Err(crate::utils::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("Duplicate property key"));
            }
            _ => panic!("Expected CorruptedData error"),
        }
    }

    #[test]
    fn test_property_map_debug_sorting() {
        // Use unsorted insert order: b, a, c
        let map = PropertyMapBuilder::new()
            .insert("b", 2)
            .insert("a", 1)
            .insert("c", 3)
            .build();

        let debug_str = format!("{:?}", map);
        // Should sort keys alphabetically: a, b, c
        // The output format is standard debug map: {"key": value, ...}
        // We look for "a": ... "b": ... "c": ... in that order
        let pos_a = debug_str.find("\"a\"").unwrap();
        let pos_b = debug_str.find("\"b\"").unwrap();
        let pos_c = debug_str.find("\"c\"").unwrap();

        assert!(
            pos_a < pos_b,
            "Debug output should be sorted: 'a' before 'b'"
        );
        assert!(
            pos_b < pos_c,
            "Debug output should be sorted: 'b' before 'c'"
        );
    }

    #[test]
    fn test_property_map_debug_fallback() {
        // Create a PropertyMap with a raw unresolved key
        // We must bypass PropertyMapBuilder because it validates keys against the interner
        let mut map = HashMap::new();
        let raw_key = InternedString::from_raw(u32::MAX);
        map.insert(raw_key, PropertyValue::Int(42));

        let prop_map = PropertyMap {
            inner: Arc::new(map),
            cached_size: 0, // Not used for Debug
        };

        let debug_str = format!("{:?}", prop_map);
        // Fallback format for unknown key: InternedString(4294967295)
        assert!(
            debug_str.contains("InternedString(4294967295)"),
            "Debug output should fallback for unknown key"
        );
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    /// 🎯 Target: PropertyMap::from_iter
    /// 💣 Risk: Panics when recursion depth limit is exceeded.
    /// 🧪 Strategy: Construct a deeply nested structure and try to create a PropertyMap from it using collect().
    /// 🔬 Verification: Expect panic with specific message.
    #[test]
    #[should_panic(expected = "Recursion depth limit exceeded in FromIterator")]
    fn test_property_map_from_iter_panics_on_deep_recursion() {
        // Construct a deeply nested value: Array(Array(...Array(Int(42))...))
        // Depth: MAX_RECURSION_DEPTH + 1
        let mut value = PropertyValue::Int(42);
        // Nest it MAX_RECURSION_DEPTH + 1 times
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        // This should panic because FromIterator uses expect() on serialized_size()
        let _map: PropertyMap = vec![(GLOBAL_INTERNER.intern("deep").unwrap(), value)]
            .into_iter()
            .collect();
    }

    /// 🎯 Target: PropertyMapBuilder::try_insert
    /// 💣 Risk: Should fail gracefully (return Err) instead of panicking on deep recursion.
    /// 🧪 Strategy: Try to insert a deeply nested structure using try_insert.
    /// 🔬 Verification: Check that Result is Err and contains the recursion limit message.
    #[test]
    fn test_property_map_builder_try_insert_returns_error_on_deep_recursion() {
        let mut value = PropertyValue::Int(42);
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        // try_insert should return an error, not panic
        let result = PropertyMapBuilder::new().try_insert("deep", value);

        assert!(result.is_err(), "Expected error, got Ok");
        let err = result.err().unwrap();
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("recursion depth limit exceeded"),
            "Unexpected error message: {}",
            err_msg
        );
    }

    /// 🎯 Target: PropertyMap::deserialize
    /// 💣 Risk: Malformed UTF-8 in property keys could cause panic or incorrect behavior.
    /// 🧪 Strategy: Manually construct a serialized buffer with invalid UTF-8 in the key section.
    /// 🔬 Verification: Verify deserialize returns a CorruptedData error with "Invalid UTF-8" message.
    #[test]
    fn test_property_map_deserialize_invalid_utf8_key() {
        // Construct a buffer manually
        // Format: [count: 4 bytes][key_len: 4 bytes][key_bytes][value...]
        let mut buffer = Vec::new();

        // Count: 1 entry
        buffer.extend_from_slice(&1u32.to_le_bytes());

        // Key length: 1 byte
        buffer.extend_from_slice(&1u32.to_le_bytes());

        // Key bytes: 0xFF is invalid in UTF-8
        buffer.push(0xFF);

        // Value: Tag Null (0), no payload
        buffer.push(TAG_NULL);

        let result = PropertyMap::deserialize(&buffer);

        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = format!("{}", err);

        // Should be StorageError::CorruptedData wrapping the utf8 error
        assert!(
            err_msg.contains("Invalid UTF-8") || err_msg.contains("Corrupted data"),
            "Unexpected error message: {}",
            err_msg
        );
    }
}
