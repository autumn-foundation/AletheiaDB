//! Property map and builder.

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasherDefault;
use std::sync::Arc;

use crate::core::error::{Result, StorageError};
use crate::core::interning::{GLOBAL_INTERNER, IdentityHasher};
use super::value::{PropertyKey, PropertyValue};

// ============================================================================
// Serialization Limits
// ============================================================================

/// Maximum capacity allowed for a deserialized property map.
/// Increased from 10K to 100K to support business scenarios:
/// - E-commerce: Products with extensive attributes and variations
/// - Scientific data: Rich metadata and measurements
/// - Dynamic schemas: User profiles with custom fields
///
///   Still provides DoS protection (~1MB per node maximum).
pub const MAX_PROPERTY_MAP_CAPACITY: usize = 100_000;

/// A map of property keys to values with copy-on-write semantics.
///
/// The underlying HashMap is wrapped in an Arc, making clones very cheap
/// (just incrementing a reference count). This enables efficient sharing
/// of unchanged properties across versions.
#[derive(Clone, PartialEq)]
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue, BuildHasherDefault<IdentityHasher>>>,
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
            inner: Arc::new(HashMap::with_hasher(BuildHasherDefault::default())),
            cached_size: 4, // 4 bytes for the count field (0)
        }
    }

    /// Create a property map with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::with_capacity_and_hasher(
                capacity,
                BuildHasherDefault::default(),
            )),
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
                    crate::core::error::Error::Storage(StorageError::InconsistentState {
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

        let mut map = HashMap::with_capacity_and_hasher(count, BuildHasherDefault::default());
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
        let mut map = HashMap::with_hasher(BuildHasherDefault::default());
        let mut size: usize = 4; // Count field

        for (key, value) in iter {
            // Need key size for serialization
            let key_len = GLOBAL_INTERNER
                .resolve_with(key, |s| s.len())
                .unwrap_or(256);
            let key_size = 4 + key_len;
            // Use penalty size if recursion limit exceeded to prevent panic.
            // Incorrect size here is safe because serialize() re-checks limits.
            // 10MB penalty discourages abuse.
            const RECURSION_PENALTY_SIZE: usize = 10 * 1024 * 1024;

            let val_size = value.serialized_size().unwrap_or(RECURSION_PENALTY_SIZE);

            size = size.saturating_add(key_size).saturating_add(val_size);

            if let Some(old_val) = map.insert(key, value) {
                // If replaced, subtract the size of the old entry (key + value)
                // Key size is the same since it's the same key ID
                size = size
                    .saturating_sub(key_size)
                    .saturating_sub(old_val.serialized_size().unwrap_or(RECURSION_PENALTY_SIZE));
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
    map: HashMap<PropertyKey, PropertyValue, BuildHasherDefault<IdentityHasher>>,
    current_size: usize,
}

impl PropertyMapBuilder {
    /// Create a new builder with an empty map.
    pub fn new() -> Self {
        PropertyMapBuilder {
            map: HashMap::with_hasher(BuildHasherDefault::default()),
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
        // Warden: Propagate interning errors (e.g. CapacityExceeded) to prevent silent data loss.
        // Previously, failure to intern would silently drop the property, which is a security risk.
        let interned_key = GLOBAL_INTERNER.intern(key)?;
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
    /// use aletheiadb::core::property::PropertyMapBuilder;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::interning::InternedString;
    use crate::core::property::value::{TAG_NULL, MAX_RECURSION_DEPTH};

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

        let map2 = map1.clone();
        assert_eq!(map1, map2);

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
    fn test_vector_in_property_map() {
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let map = PropertyMapBuilder::new()
            .insert("name", "test_node")
            .insert("embedding", PropertyValue::vector(&embedding))
            .build();

        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_serialize_property_map_empty() {
        let map = PropertyMap::new();
        let bytes = map.serialize().expect("Serialization should succeed");

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
        let result = PropertyMap::deserialize(&[]);
        assert!(result.is_err());

        let result = PropertyMap::deserialize(&[1, 2]);
        assert!(result.is_err());

        let result = PropertyMap::deserialize(&[1, 0, 0, 0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_property_key_interning_serialization_round_trip() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .insert("active", true)
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");

        let (deserialized, _) =
            PropertyMap::deserialize(&bytes).expect("Deserialization should succeed");

        assert_eq!(
            deserialized.get("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
        assert_eq!(deserialized.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(
            deserialized.get("active").and_then(|v| v.as_bool()),
            Some(true)
        );

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

        assert_eq!(
            size_of::<InternedString>(),
            4,
            "InternedString should be 4 bytes"
        );
        assert_eq!(size_of::<String>(), 24, "String should be 24 bytes");

        let maps: Vec<_> = (0..100)
            .map(|i| {
                PropertyMapBuilder::new()
                    .insert("test_mem_name", format!("Person{}", i))
                    .insert("test_mem_age", i as i64)
                    .insert("test_mem_id", i as i64)
                    .build()
            })
            .collect();

        use std::collections::HashSet;
        let first_keys: HashSet<_> = maps[0].keys().cloned().collect();
        for map in &maps[1..] {
            let map_keys: HashSet<_> = map.keys().cloned().collect();
            assert_eq!(
                first_keys, map_keys,
                "All maps should share the same interned key IDs"
            );
        }

        assert_eq!(first_keys.len(), 3, "Should have exactly 3 unique keys");

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
        let invalid_key = InternedString::from_raw(999999);

        let mut inner_map = HashMap::with_hasher(BuildHasherDefault::default());
        inner_map.insert(invalid_key, PropertyValue::Int(42));
        let map = PropertyMap {
            inner: Arc::new(inner_map),
            cached_size: 4,
        };

        let result = map.serialize();
        assert!(
            result.is_err(),
            "Serialization should fail for invalid InternedString"
        );

        match result {
            Err(crate::core::error::Error::Storage(StorageError::InconsistentState { reason })) => {
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
        let invalid_key = InternedString::from_raw(888888);
        let mut inner_map = HashMap::with_hasher(BuildHasherDefault::default());
        inner_map.insert(invalid_key, PropertyValue::Bool(true));
        let map = PropertyMap {
            inner: Arc::new(inner_map),
            cached_size: 4,
        };

        let mut buffer = Vec::new();
        let result = map.serialize_into(&mut buffer);

        assert!(result.is_err(), "Should return error for missing key");
        match result {
            Err(crate::core::error::Error::Storage(StorageError::InconsistentState { reason })) => {
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

        let map = Arc::new(
            PropertyMapBuilder::new()
                .insert("shared_key", "value")
                .insert("count", 0i64)
                .build(),
        );

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let map_clone = Arc::clone(&map);
                thread::spawn(move || {
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

        for handle in handles {
            handle.join().expect("Thread should complete successfully");
        }
    }

    #[test]
    fn test_property_key_get_efficiency() {
        let interned_key = GLOBAL_INTERNER.intern("test_key").unwrap();

        let map = PropertyMapBuilder::new()
            .insert("test_key", "value")
            .build();

        let value1 = map.get("test_key");
        assert_eq!(value1.and_then(|v| v.as_str()), Some("value"));

        let value2 = map.get_by_interned_key(&interned_key);
        assert_eq!(value2.and_then(|v| v.as_str()), Some("value"));

        assert_eq!(value1, value2);
    }

    #[test]
    fn test_sparse_vector_in_property_map() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![10, 42, 100], vec![2.5, 1.8, 3.2], 1000).unwrap();
        let map = PropertyMapBuilder::new()
            .insert("name", "document1")
            .insert("sparse_embedding", PropertyValue::sparse_vector(sparse))
            .build();

        assert_eq!(map.len(), 2);
        let retrieved_sparse = map
            .get("sparse_embedding")
            .and_then(|v| v.as_sparse_vector());
        assert!(retrieved_sparse.is_some());
        assert_eq!(retrieved_sparse.unwrap().nnz(), 3);
        assert_eq!(retrieved_sparse.unwrap().dimension(), 1000);
    }

    #[test]
    fn test_property_map_contains_vector_with_sparse() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0], vec![1.0], 10).unwrap();
        let map = PropertyMapBuilder::new()
            .insert("sparse", PropertyValue::sparse_vector(sparse))
            .build();
        assert!(map.contains_vector());

        let map = PropertyMapBuilder::new()
            .insert("dense", PropertyValue::vector([1.0f32, 2.0, 3.0]))
            .build();
        assert!(map.contains_vector());

        let map = PropertyMapBuilder::new()
            .insert("name", "test")
            .insert("count", 42i64)
            .build();
        assert!(!map.contains_vector());
    }

    #[test]
    fn test_serialize_sparse_vector_in_property_map() {
        use crate::core::vector::SparseVec;

        let sparse = SparseVec::new(vec![0, 5, 10], vec![1.0, 2.0, 3.0], 20).unwrap();
        let map = PropertyMapBuilder::new()
            .insert("id", 123i64)
            .insert("sparse_vec", PropertyValue::sparse_vector(sparse))
            .build();

        let bytes = map.serialize().expect("Serialization should succeed");
        let (deserialized, _) = PropertyMap::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.get("id").and_then(|v| v.as_int()), Some(123));

        let sparse_result = deserialized
            .get("sparse_vec")
            .and_then(|v| v.as_sparse_vector());
        assert!(sparse_result.is_some());
        let sv = sparse_result.unwrap();
        assert_eq!(sv.nnz(), 3);
        assert_eq!(sv.dimension(), 20);
    }

    #[test]
    fn test_property_map_estimated_heap_size_empty() {
        let map = PropertyMap::new();
        let size = map.estimated_heap_size();
        assert_eq!(size, 0, "Empty map heap size should be zero");
    }

    #[test]
    fn test_property_map_estimated_heap_size_with_values() {
        let map_empty = PropertyMap::with_capacity(100);
        let empty_size = map_empty.estimated_heap_size();

        let large_string = "x".repeat(10000);
        let map_with_string = map_empty
            .builder()
            .insert("large", large_string.as_str())
            .build();
        let string_size = map_with_string.estimated_heap_size();

        let delta = string_size - empty_size;
        assert_eq!(
            delta,
            large_string.len(),
            "Heap size delta should be exactly string length (assuming no resize)"
        );
    }

    #[test]
    fn test_property_map_estimated_heap_size_with_vector() {
        let map_empty = PropertyMap::with_capacity(100);
        let empty_size = map_empty.estimated_heap_size();

        let embedding = vec![0.1f32; 384];
        let map_with_vector = map_empty
            .builder()
            .insert_vector("embedding", &embedding)
            .build();
        let vector_size = map_with_vector.estimated_heap_size();

        let delta = vector_size - empty_size;
        let expected_delta = embedding.len() * std::mem::size_of::<f32>();

        assert_eq!(
            delta, expected_delta,
            "Heap size delta should be exactly vector data size"
        );
    }

    #[test]
    fn test_property_map_serialized_size() {
        let map = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let expected_size = 4 + 18 + 16;
        assert_eq!(
            map.serialized_size(),
            expected_size,
            "Serialized size should match manual calculation"
        );

        let actual = map.serialize().unwrap().len();
        assert_eq!(expected_size, actual);
    }

    #[test]
    fn test_concurrent_property_map_creation() {
        use std::thread;

        let handles: Vec<_> = (0..10)
            .map(|i| {
                thread::spawn(move || {
                    let mut builder = PropertyMapBuilder::new();
                    builder = builder.insert("shared_key_1", "value1");
                    builder = builder.insert("shared_key_2", 42i64);

                    let unique_key = format!("unique_key_{}", i);
                    builder = builder.insert(&unique_key, i as i64);

                    let map = builder.build();
                    assert_eq!(map.len(), 3);
                    assert_eq!(
                        map.get("shared_key_1").and_then(|v| v.as_str()),
                        Some("value1")
                    );
                    assert_eq!(
                        map.get(&unique_key).and_then(|v| v.as_int()),
                        Some(i as i64)
                    );
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_cached_size_tracking() {
        let builder = PropertyMapBuilder::new();

        let map = builder.build();
        assert_eq!(map.serialized_size(), 4);

        let builder = map.builder().insert("key1", 100i64);
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());

        let builder = map.builder().insert("key2", "hello");
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());

        let builder = map.builder().insert("key1", 200i64);
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());

        let builder = map.builder().remove("key2");
        let map = builder.build();
        assert_eq!(map.cached_size, map.serialized_size());
    }

    #[test]
    fn test_cached_size_invariant() {
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
        let items = vec![
            (
                GLOBAL_INTERNER.intern("key").unwrap(),
                PropertyValue::Int(1),
            ),
            (
                GLOBAL_INTERNER.intern("key").unwrap(),
                PropertyValue::Int(2),
            ),
        ];
        let map: PropertyMap = items.into_iter().collect();

        let serialized = map.serialize().unwrap();
        assert_eq!(map.cached_size, serialized.len());
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("key").and_then(|v| v.as_int()), Some(2));
    }

    #[test]
    fn test_deserialize_duplicate_keys_errors() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&2u32.to_le_bytes());

        let key = "a";
        buffer.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buffer.extend_from_slice(key.as_bytes());
        PropertyValue::Int(1).serialize_into(&mut buffer).unwrap();

        buffer.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buffer.extend_from_slice(key.as_bytes());
        PropertyValue::Int(2).serialize_into(&mut buffer).unwrap();

        let result = PropertyMap::deserialize(&buffer);
        assert!(result.is_err());
        match result {
            Err(crate::core::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(msg.contains("Duplicate property key"));
            }
            _ => panic!("Expected CorruptedData error"),
        }
    }

    #[test]
    fn test_property_map_debug_sorting() {
        let map = PropertyMapBuilder::new()
            .insert("b", 2)
            .insert("a", 1)
            .insert("c", 3)
            .build();

        let debug_str = format!("{:?}", map);
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
        let mut map = HashMap::with_hasher(BuildHasherDefault::default());
        let raw_key = InternedString::from_raw(u32::MAX);
        map.insert(raw_key, PropertyValue::Int(42));

        let prop_map = PropertyMap {
            inner: Arc::new(map),
            cached_size: 0,
        };

        let debug_str = format!("{:?}", prop_map);
        assert!(
            debug_str.contains("InternedString(4294967295)"),
            "Debug output should fallback for unknown key"
        );
    }

    #[test]
    fn test_property_map_from_iter_no_panic_on_deep_recursion() {
        let mut value = PropertyValue::Int(42);
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

        let map: PropertyMap = vec![(GLOBAL_INTERNER.intern("deep").unwrap(), value)]
            .into_iter()
            .collect();

        let result = map.serialize();
        assert!(
            result.is_err(),
            "Serialization should fail due to recursion limit"
        );

        assert!(map.serialized_size() > 10 * 1024 * 1024);
    }

    #[test]
    fn test_property_map_builder_try_insert_returns_error_on_deep_recursion() {
        let mut value = PropertyValue::Int(42);
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }

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

    #[test]
    fn test_property_map_deserialize_invalid_utf8_key() {
        let mut buffer = Vec::new();

        buffer.extend_from_slice(&1u32.to_le_bytes());

        buffer.extend_from_slice(&1u32.to_le_bytes());

        buffer.push(0xFF);

        buffer.push(TAG_NULL);

        let result = PropertyMap::deserialize(&buffer);

        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = format!("{}", err);

        assert!(
            err_msg.contains("Invalid UTF-8") || err_msg.contains("Corrupted data"),
            "Unexpected error message: {}",
            err_msg
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "missing from interner")]
    fn test_builder_insert_by_key_panic() {
        let invalid_key = InternedString::from_raw(u32::MAX);
        let builder = PropertyMapBuilder::new();
        builder.insert_by_key(invalid_key, PropertyValue::Int(1));
    }

    #[test]
    #[should_panic(expected = "recursion depth limit exceeded")]
    fn test_property_map_builder_insert_panic_message() {
        let mut value = PropertyValue::Int(42);
        for _ in 0..MAX_RECURSION_DEPTH + 1 {
            value = PropertyValue::Array(Arc::new(vec![value]));
        }
        PropertyMapBuilder::new().insert("deep", value);
    }

    #[test]
    fn test_property_map_capacity_limit() {
        let mut bytes = Vec::new();
        let count = (MAX_PROPERTY_MAP_CAPACITY + 1) as u32;
        bytes.extend_from_slice(&count.to_le_bytes());

        let result = PropertyMap::deserialize(&bytes);
        assert!(result.is_err());
        match result {
            Err(crate::core::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(
                    msg.contains("exceeds maximum allowed"),
                    "Unexpected error message: {}",
                    msg
                );
            }
            _ => panic!("Expected CorruptedData error"),
        }
    }

    #[test]
    fn test_property_map_builder_remove_correctness() {
        let builder = PropertyMapBuilder::new()
            .insert("keep", 1)
            .insert("remove_me", 2);

        let map_before = builder.build();
        assert_eq!(map_before.len(), 2);
        assert!(map_before.contains_key("remove_me"));

        let before_size = map_before.serialized_size();

        let builder = map_before.clone().builder().remove("remove_me");
        let map_after = builder.build();

        assert_eq!(map_after.len(), 1);
        assert!(!map_after.contains_key("remove_me"));
        assert!(map_after.contains_key("keep"));

        assert!(map_after.serialized_size() < before_size);
        assert_eq!(
            map_after.serialized_size(),
            map_after.serialize().unwrap().len()
        );
    }

    #[test]
    fn test_property_map_deserialize_trailing_bytes() {
        let map = PropertyMapBuilder::new().insert("key", "value").build();
        let mut bytes = map.serialize().unwrap();
        let expected_size = bytes.len();

        bytes.extend_from_slice(&[0xFF, 0xEE, 0xDD]);

        let (deserialized, consumed) = PropertyMap::deserialize(&bytes).unwrap();

        assert_eq!(deserialized, map);
        assert_eq!(consumed, expected_size);
        assert!(consumed < bytes.len());
    }

    #[test]
    fn test_property_map_deserialize_insufficient_buffer_preallocation() {
        let mut bytes = Vec::new();
        let count = 50_000u32;
        bytes.extend_from_slice(&count.to_le_bytes());

        let result = PropertyMap::deserialize(&bytes);
        assert!(result.is_err());
        match result {
            Err(crate::core::error::Error::Storage(StorageError::CorruptedData(msg))) => {
                assert!(
                    msg.contains("Insufficient buffer size"),
                    "Unexpected error message: {}",
                    msg
                );
            }
            _ => panic!("Expected CorruptedData error"),
        }
    }

    #[test]
    fn test_property_map_capacity_boundary() {
        let mut bytes = Vec::new();
        let count = MAX_PROPERTY_MAP_CAPACITY as u32;
        bytes.extend_from_slice(&count.to_le_bytes());

        let result = PropertyMap::deserialize(&bytes);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Insufficient buffer size")
        );

        let mut bytes_overflow = Vec::new();
        let count_overflow = (MAX_PROPERTY_MAP_CAPACITY + 1) as u32;
        bytes_overflow.extend_from_slice(&count_overflow.to_le_bytes());

        let result_overflow = PropertyMap::deserialize(&bytes_overflow);
        assert!(result_overflow.is_err());
        assert!(
            result_overflow
                .unwrap_err()
                .to_string()
                .contains("exceeds maximum allowed")
        );
    }

    #[test]
    fn test_contains_vector_nested() {
        let embedding = vec![0.1f32; 4];
        let vec_val = PropertyValue::vector(&embedding);
        let array_val = PropertyValue::array(vec![vec_val]);

        let map = PropertyMapBuilder::new()
            .insert("nested_vector", array_val)
            .build();

        assert!(
            !map.contains_vector(),
            "contains_vector should ignore nested vectors (current limitation)"
        );
    }

    #[test]
    fn test_concurrent_interning_stress() {
        use std::sync::Arc;
        use std::thread;

        const NUM_THREADS: usize = 10;
        const NUM_KEYS: usize = 100;

        let barrier = Arc::new(std::sync::Barrier::new(NUM_THREADS));

        let handles: Vec<_> = (0..NUM_THREADS)
            .map(|t_id| {
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();

                    let mut builder = PropertyMapBuilder::new();
                    for i in 0..NUM_KEYS {
                        let key = format!("concurrent_stress_key_{}", i);
                        builder = builder.insert(&key, (t_id * 1000 + i) as i64);
                    }
                    let map = builder.build();

                    assert_eq!(map.len(), NUM_KEYS);
                    for i in 0..NUM_KEYS {
                        let key = format!("concurrent_stress_key_{}", i);
                        assert_eq!(
                            map.get(&key).and_then(|v| v.as_int()),
                            Some((t_id * 1000 + i) as i64)
                        );
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
