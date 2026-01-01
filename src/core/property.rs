//! Property system with Arc-based deduplication.
//!
//! This module provides a copy-on-write property system where properties are
//! stored in immutable, reference-counted containers. This enables:
//! - Cheap cloning of property maps (just increment reference count)
//! - Deduplication of unchanged properties across versions
//! - Zero-copy sharing of immutable data

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::utils::error::{Result, StorageError};

// ============================================================================
// Serialization Type Tags
// ============================================================================
// Binary format uses little-endian byte order (consistent with WAL format).
// Each PropertyValue variant has a unique 1-byte type tag.

/// Type tag for Null value.
pub const TAG_NULL: u8 = 0;
/// Type tag for Bool value.
pub const TAG_BOOL: u8 = 1;
/// Type tag for Int (i64) value.
pub const TAG_INT: u8 = 2;
/// Type tag for Float (f64) value.
pub const TAG_FLOAT: u8 = 3;
/// Type tag for String value.
pub const TAG_STRING: u8 = 4;
/// Type tag for Bytes value.
pub const TAG_BYTES: u8 = 5;
/// Type tag for Array value.
pub const TAG_ARRAY: u8 = 6;
/// Type tag for Vector (dense f32 array) value.
pub const TAG_VECTOR: u8 = 7;

/// Property key type.
///
/// TODO: Replace with InternedString once string interning is implemented.
/// For now, using String directly.
pub type PropertyKey = String;

/// A value that can be stored as a property.
///
/// All complex types (strings, bytes, arrays) use Arc for cheap cloning and sharing.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// Null/absent value.
    Null,
    /// Boolean value.
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit floating point.
    Float(f64),
    /// UTF-8 string (reference counted).
    String(Arc<str>),
    /// Byte array (reference counted).
    Bytes(Arc<[u8]>),
    /// Array of values (reference counted).
    Array(Arc<Vec<PropertyValue>>),
    /// Dense vector for embeddings (reference counted).
    /// Uses f32 for memory efficiency - standard for ML embeddings.
    ///
    /// # Floating-Point Equality Note
    /// This variant uses derived PartialEq which compares f32 values bitwise.
    /// Be aware that NaN != NaN (IEEE 754) and floating-point precision may
    /// cause semantically equal vectors to compare unequal. For similarity
    /// comparisons, use dedicated vector utility functions (e.g., cosine similarity)
    /// rather than equality. This limitation will be revisited in Phase 3 when
    /// vectors are used in temporal storage for deduplication.
    Vector(Arc<[f32]>),
}

impl PropertyValue {
    /// Create a string property value from a &str.
    pub fn string<S: AsRef<str>>(s: S) -> Self {
        PropertyValue::String(Arc::from(s.as_ref()))
    }

    /// Create a bytes property value from a slice.
    pub fn bytes<B: AsRef<[u8]>>(b: B) -> Self {
        PropertyValue::Bytes(Arc::from(b.as_ref()))
    }

    /// Create an array property value from a Vec.
    pub fn array(values: Vec<PropertyValue>) -> Self {
        PropertyValue::Array(Arc::new(values))
    }

    /// Create a vector property value from a slice.
    ///
    /// Dense vectors are used for embeddings in vector search.
    /// The data is stored in an Arc for efficient cloning and sharing.
    pub fn vector<V: AsRef<[f32]>>(v: V) -> Self {
        PropertyValue::Vector(Arc::from(v.as_ref()))
    }

    /// Returns true if this value is null.
    #[inline]
    pub const fn is_null(&self) -> bool {
        matches!(self, PropertyValue::Null)
    }

    /// Try to get this value as a bool.
    #[inline]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            PropertyValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Try to get this value as an integer.
    #[inline]
    pub const fn as_int(&self) -> Option<i64> {
        match self {
            PropertyValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Try to get this value as a float.
    #[inline]
    pub const fn as_float(&self) -> Option<f64> {
        match self {
            PropertyValue::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Try to get this value as a string reference.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropertyValue::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    /// Try to get this value as a byte slice.
    #[inline]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            PropertyValue::Bytes(b) => Some(b.as_ref()),
            _ => None,
        }
    }

    /// Try to get this value as an array.
    #[inline]
    pub fn as_array(&self) -> Option<&[PropertyValue]> {
        match self {
            PropertyValue::Array(a) => Some(a.as_ref()),
            _ => None,
        }
    }

    /// Try to get this value as a vector (dense embedding).
    #[inline]
    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            PropertyValue::Vector(v) => Some(v.as_ref()),
            _ => None,
        }
    }

    /// Get the type name of this value.
    pub const fn type_name(&self) -> &'static str {
        match self {
            PropertyValue::Null => "null",
            PropertyValue::Bool(_) => "bool",
            PropertyValue::Int(_) => "int",
            PropertyValue::Float(_) => "float",
            PropertyValue::String(_) => "string",
            PropertyValue::Bytes(_) => "bytes",
            PropertyValue::Array(_) => "array",
            PropertyValue::Vector(_) => "vector",
        }
    }

    // ========================================================================
    // Serialization Methods
    // ========================================================================

    /// Serialize this PropertyValue to bytes.
    ///
    /// # Binary Format
    /// - Tag (1 byte): Identifies the value type
    /// - Payload: Type-specific data in little-endian format
    ///
    /// | Type   | Format                                      |
    /// |--------|---------------------------------------------|
    /// | Null   | [tag:1]                                     |
    /// | Bool   | [tag:1][value:1]                           |
    /// | Int    | [tag:1][i64:8]                             |
    /// | Float  | [tag:1][f64:8]                             |
    /// | String | [tag:1][len:4][utf8_bytes:len]             |
    /// | Bytes  | [tag:1][len:4][bytes:len]                  |
    /// | Array  | [tag:1][count:4][elements...]              |
    /// | Vector | [tag:1][dim:4][f32_values:dim*4]           |
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        self.serialize_into(&mut buffer);
        buffer
    }

    /// Serialize this PropertyValue into an existing buffer.
    ///
    /// This is more efficient when serializing multiple values as it avoids
    /// allocating a new Vec for each value.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) {
        match self {
            PropertyValue::Null => {
                buffer.push(TAG_NULL);
            }
            PropertyValue::Bool(b) => {
                buffer.push(TAG_BOOL);
                buffer.push(if *b { 1 } else { 0 });
            }
            PropertyValue::Int(i) => {
                buffer.push(TAG_INT);
                buffer.extend_from_slice(&i.to_le_bytes());
            }
            PropertyValue::Float(f) => {
                buffer.push(TAG_FLOAT);
                buffer.extend_from_slice(&f.to_le_bytes());
            }
            PropertyValue::String(s) => {
                buffer.push(TAG_STRING);
                let bytes = s.as_bytes();
                buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buffer.extend_from_slice(bytes);
            }
            PropertyValue::Bytes(b) => {
                buffer.push(TAG_BYTES);
                buffer.extend_from_slice(&(b.len() as u32).to_le_bytes());
                buffer.extend_from_slice(b);
            }
            PropertyValue::Array(arr) => {
                buffer.push(TAG_ARRAY);
                buffer.extend_from_slice(&(arr.len() as u32).to_le_bytes());
                for item in arr.iter() {
                    item.serialize_into(buffer);
                }
            }
            PropertyValue::Vector(v) => {
                serialize_vector_into(v, buffer);
            }
        }
    }

    /// Deserialize a PropertyValue from bytes.
    ///
    /// Returns the deserialized value and the number of bytes consumed.
    pub fn deserialize(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.is_empty() {
            return Err(StorageError::CorruptedData(
                "Empty buffer when deserializing PropertyValue".to_string(),
            )
            .into());
        }

        let tag = bytes[0];
        let mut offset = 1;

        match tag {
            TAG_NULL => Ok((PropertyValue::Null, offset)),

            TAG_BOOL => {
                if bytes.len() < 2 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Bool value".to_string(),
                    )
                    .into());
                }
                let value = bytes[1] != 0;
                Ok((PropertyValue::Bool(value), 2))
            }

            TAG_INT => {
                if bytes.len() < 9 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Int value".to_string(),
                    )
                    .into());
                }
                let value = i64::from_le_bytes([
                    bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
                ]);
                Ok((PropertyValue::Int(value), 9))
            }

            TAG_FLOAT => {
                if bytes.len() < 9 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Float value".to_string(),
                    )
                    .into());
                }
                let value = f64::from_le_bytes([
                    bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
                ]);
                Ok((PropertyValue::Float(value), 9))
            }

            TAG_STRING => {
                if bytes.len() < 5 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for String length".to_string(),
                    )
                    .into());
                }
                let len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                offset = 5;

                if bytes.len() < offset + len {
                    return Err(StorageError::CorruptedData(format!(
                        "Buffer too short for String data: need {} bytes, have {}",
                        offset + len,
                        bytes.len()
                    ))
                    .into());
                }

                let string_data = &bytes[offset..offset + len];
                let s = std::str::from_utf8(string_data).map_err(|e| {
                    StorageError::CorruptedData(format!("Invalid UTF-8 in String: {}", e))
                })?;
                Ok((PropertyValue::String(Arc::from(s)), offset + len))
            }

            TAG_BYTES => {
                if bytes.len() < 5 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Bytes length".to_string(),
                    )
                    .into());
                }
                let len = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                offset = 5;

                if bytes.len() < offset + len {
                    return Err(StorageError::CorruptedData(format!(
                        "Buffer too short for Bytes data: need {} bytes, have {}",
                        offset + len,
                        bytes.len()
                    ))
                    .into());
                }

                let byte_data = &bytes[offset..offset + len];
                Ok((PropertyValue::Bytes(Arc::from(byte_data)), offset + len))
            }

            TAG_ARRAY => {
                if bytes.len() < 5 {
                    return Err(StorageError::CorruptedData(
                        "Buffer too short for Array count".to_string(),
                    )
                    .into());
                }
                let count = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                offset = 5;

                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    if offset >= bytes.len() {
                        return Err(StorageError::CorruptedData(
                            "Buffer exhausted while reading Array elements".to_string(),
                        )
                        .into());
                    }
                    let (item, consumed) = PropertyValue::deserialize(&bytes[offset..])?;
                    items.push(item);
                    offset += consumed;
                }
                Ok((PropertyValue::Array(Arc::new(items)), offset))
            }

            TAG_VECTOR => {
                let (vector, consumed) = deserialize_vector(bytes)?;
                Ok((PropertyValue::Vector(vector), consumed))
            }

            _ => Err(StorageError::CorruptedData(format!(
                "Unknown PropertyValue type tag: {}",
                tag
            ))
            .into()),
        }
    }
}

// ============================================================================
// Vector Serialization Functions
// ============================================================================

/// Serialize a vector (dense f32 array) to bytes.
///
/// # Binary Format
/// ```text
/// [tag:1][dimension:4][f32_0:4][f32_1:4]...[f32_n:4]
/// ```
///
/// - Tag: TAG_VECTOR (7)
/// - Dimension: u32 little-endian, number of elements
/// - Values: f32 little-endian, the vector elements
///
/// # Arguments
/// * `v` - The vector data to serialize
///
/// # Returns
/// A Vec<u8> containing the serialized vector
///
/// # Example
/// ```ignore
/// let embedding = [0.1f32, 0.2, 0.3];
/// let bytes = serialize_vector(&embedding);
/// // bytes = [7, 3, 0, 0, 0, <12 bytes of f32 data>]
/// ```
pub fn serialize_vector(v: &[f32]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(1 + 4 + v.len() * 4);
    serialize_vector_into(v, &mut buffer);
    buffer
}

/// Serialize a vector into an existing buffer.
///
/// This is more efficient when serializing as part of a larger structure.
pub fn serialize_vector_into(v: &[f32], buffer: &mut Vec<u8>) {
    buffer.push(TAG_VECTOR);
    buffer.extend_from_slice(&(v.len() as u32).to_le_bytes());
    for &value in v {
        buffer.extend_from_slice(&value.to_le_bytes());
    }
}

/// Deserialize a vector from bytes.
///
/// # Binary Format
/// Expects the format produced by `serialize_vector`:
/// ```text
/// [tag:1][dimension:4][f32_values:dimension*4]
/// ```
///
/// # Arguments
/// * `bytes` - The byte slice to deserialize from
///
/// # Returns
/// * `Ok((Arc<[f32]>, usize))` - The deserialized vector and bytes consumed
/// * `Err` - If the data is malformed or truncated
///
/// # Errors
/// - `StorageError::CorruptedData` if buffer is too short
/// - `StorageError::CorruptedData` if type tag is not TAG_VECTOR
///
/// # Example
/// ```ignore
/// let bytes = serialize_vector(&[0.1f32, 0.2, 0.3]);
/// let (vector, consumed) = deserialize_vector(&bytes)?;
/// assert_eq!(vector.as_ref(), &[0.1f32, 0.2, 0.3]);
/// ```
pub fn deserialize_vector(bytes: &[u8]) -> Result<(Arc<[f32]>, usize)> {
    // Need at least tag (1) + dimension (4) = 5 bytes
    if bytes.len() < 5 {
        return Err(
            StorageError::CorruptedData("Buffer too short for vector header".to_string()).into(),
        );
    }

    let tag = bytes[0];
    if tag != TAG_VECTOR {
        return Err(StorageError::CorruptedData(format!(
            "Expected vector type tag {}, got {}",
            TAG_VECTOR, tag
        ))
        .into());
    }

    let dimension = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
    let data_start = 5;
    let data_len = dimension * 4;
    let total_len = data_start + data_len;

    if bytes.len() < total_len {
        return Err(StorageError::CorruptedData(format!(
            "Buffer too short for vector data: need {} bytes, have {}",
            total_len,
            bytes.len()
        ))
        .into());
    }

    // Deserialize f32 values
    let mut values = Vec::with_capacity(dimension);
    let mut offset = data_start;
    for _ in 0..dimension {
        let value = f32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
        values.push(value);
        offset += 4;
    }

    Ok((Arc::from(values.into_boxed_slice()), total_len))
}

impl fmt::Display for PropertyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PropertyValue::Null => write!(f, "null"),
            PropertyValue::Bool(b) => write!(f, "{}", b),
            PropertyValue::Int(i) => write!(f, "{}", i),
            PropertyValue::Float(fl) => write!(f, "{}", fl),
            PropertyValue::String(s) => write!(f, "\"{}\"", s),
            PropertyValue::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            PropertyValue::Array(a) => {
                write!(f, "[")?;
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
            PropertyValue::Vector(v) => write!(f, "<vector[{}]>", v.len()),
        }
    }
}

// Convenient From implementations
impl From<bool> for PropertyValue {
    fn from(b: bool) -> Self {
        PropertyValue::Bool(b)
    }
}

impl From<i64> for PropertyValue {
    fn from(i: i64) -> Self {
        PropertyValue::Int(i)
    }
}

impl From<i32> for PropertyValue {
    fn from(i: i32) -> Self {
        PropertyValue::Int(i as i64)
    }
}

impl From<f64> for PropertyValue {
    fn from(f: f64) -> Self {
        PropertyValue::Float(f)
    }
}

impl From<String> for PropertyValue {
    fn from(s: String) -> Self {
        PropertyValue::String(Arc::from(s.as_str()))
    }
}

impl From<&str> for PropertyValue {
    fn from(s: &str) -> Self {
        PropertyValue::String(Arc::from(s))
    }
}

impl From<Vec<u8>> for PropertyValue {
    fn from(b: Vec<u8>) -> Self {
        PropertyValue::Bytes(Arc::from(b.as_slice()))
    }
}

impl From<&[u8]> for PropertyValue {
    fn from(b: &[u8]) -> Self {
        PropertyValue::Bytes(Arc::from(b))
    }
}

impl From<Vec<PropertyValue>> for PropertyValue {
    fn from(v: Vec<PropertyValue>) -> Self {
        PropertyValue::Array(Arc::new(v))
    }
}

impl From<Vec<f32>> for PropertyValue {
    fn from(v: Vec<f32>) -> Self {
        // Use v.into() to reuse the Vec's buffer, avoiding allocation and copy
        PropertyValue::Vector(v.into())
    }
}

impl From<&[f32]> for PropertyValue {
    fn from(v: &[f32]) -> Self {
        PropertyValue::Vector(Arc::from(v))
    }
}

/// A map of property keys to values with copy-on-write semantics.
///
/// The underlying HashMap is wrapped in an Arc, making clones very cheap
/// (just incrementing a reference count). This enables efficient sharing
/// of unchanged properties across versions.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyMap {
    inner: Arc<HashMap<PropertyKey, PropertyValue>>,
}

impl PropertyMap {
    /// Create a new empty property map.
    pub fn new() -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::new()),
        }
    }

    /// Create a property map with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        PropertyMap {
            inner: Arc::new(HashMap::with_capacity(capacity)),
        }
    }

    /// Get a property value by key.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&PropertyValue> {
        self.inner.get(key)
    }

    /// Check if a property exists.
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
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
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        self.serialize_into(&mut buffer);
        buffer
    }

    /// Serialize this PropertyMap into an existing buffer.
    pub fn serialize_into(&self, buffer: &mut Vec<u8>) {
        buffer.extend_from_slice(&(self.inner.len() as u32).to_le_bytes());
        for (key, value) in self.inner.iter() {
            // Serialize key
            let key_bytes = key.as_bytes();
            buffer.extend_from_slice(&(key_bytes.len() as u32).to_le_bytes());
            buffer.extend_from_slice(key_bytes);
            // Serialize value
            value.serialize_into(buffer);
        }
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

        let count = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let mut offset = 4;
        let mut map = HashMap::with_capacity(count);

        for _ in 0..count {
            // Read key length
            if bytes.len() < offset + 4 {
                return Err(StorageError::CorruptedData(
                    "Buffer too short for property key length".to_string(),
                )
                .into());
            }
            let key_len = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]) as usize;
            offset += 4;

            // Read key
            if bytes.len() < offset + key_len {
                return Err(StorageError::CorruptedData(
                    "Buffer too short for property key data".to_string(),
                )
                .into());
            }
            let key = std::str::from_utf8(&bytes[offset..offset + key_len])
                .map_err(|e| {
                    StorageError::CorruptedData(format!("Invalid UTF-8 in property key: {}", e))
                })?
                .to_string();
            offset += key_len;

            // Read value
            let (value, consumed) = PropertyValue::deserialize(&bytes[offset..])?;
            offset += consumed;

            map.insert(key, value);
        }

        Ok((
            PropertyMap {
                inner: Arc::new(map),
            },
            offset,
        ))
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(PropertyKey, PropertyValue)> for PropertyMap {
    fn from_iter<I: IntoIterator<Item = (PropertyKey, PropertyValue)>>(iter: I) -> Self {
        PropertyMap {
            inner: Arc::new(iter.into_iter().collect()),
        }
    }
}

/// Builder for creating or modifying property maps with copy-on-write semantics.
pub struct PropertyMapBuilder {
    map: HashMap<PropertyKey, PropertyValue>,
}

impl PropertyMapBuilder {
    /// Create a new builder with an empty map.
    pub fn new() -> Self {
        PropertyMapBuilder {
            map: HashMap::new(),
        }
    }

    /// Create a builder from an existing PropertyMap.
    ///
    /// This will clone the underlying HashMap if the Arc has multiple references,
    /// implementing copy-on-write semantics.
    pub fn from_map(prop_map: PropertyMap) -> Self {
        let map = Arc::try_unwrap(prop_map.inner).unwrap_or_else(|arc| (*arc).clone());
        PropertyMapBuilder { map }
    }

    /// Insert a property.
    pub fn insert<K: Into<PropertyKey>, V: Into<PropertyValue>>(
        mut self,
        key: K,
        value: V,
    ) -> Self {
        self.map.insert(key.into(), value.into());
        self
    }

    /// Remove a property.
    pub fn remove(mut self, key: &str) -> Self {
        self.map.remove(key);
        self
    }

    /// Build the final PropertyMap.
    pub fn build(self) -> PropertyMap {
        PropertyMap {
            inner: Arc::new(self.map),
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

    #[test]
    fn test_property_value_types() {
        assert!(PropertyValue::Null.is_null());
        assert_eq!(PropertyValue::Bool(true).as_bool(), Some(true));
        assert_eq!(PropertyValue::Int(42).as_int(), Some(42));
        assert_eq!(PropertyValue::Float(2.5).as_float(), Some(2.5));

        let s = PropertyValue::string("hello");
        assert_eq!(s.as_str(), Some("hello"));

        let b = PropertyValue::bytes([1, 2, 3]);
        assert_eq!(b.as_bytes(), Some(&[1u8, 2, 3][..]));

        let arr = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        assert_eq!(arr.as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_property_value_from() {
        let _: PropertyValue = true.into();
        let _: PropertyValue = 42i64.into();
        let _: PropertyValue = 42i32.into();
        let _: PropertyValue = 2.5f64.into();
        let _: PropertyValue = "hello".into();
        let _: PropertyValue = String::from("world").into();
        let _: PropertyValue = vec![1u8, 2, 3].into();
    }

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
        assert!(keys.contains(&"a".to_string()));
        assert!(keys.contains(&"b".to_string()));
        assert!(keys.contains(&"c".to_string()));

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
    fn test_property_value_display() {
        assert_eq!(format!("{}", PropertyValue::Null), "null");
        assert_eq!(format!("{}", PropertyValue::Bool(true)), "true");
        assert_eq!(format!("{}", PropertyValue::Int(42)), "42");
        assert_eq!(format!("{}", PropertyValue::Float(2.5)), "2.5");
        assert_eq!(format!("{}", PropertyValue::string("hello")), "\"hello\"");

        let arr = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        assert_eq!(format!("{}", arr), "[1, 2]");
    }

    #[test]
    fn test_arc_sharing() {
        // Create a property value with a large string
        let large_string = "x".repeat(1000);
        let prop1 = PropertyValue::string(&large_string);
        let prop2 = prop1.clone();

        // Both should point to the same Arc
        if let (PropertyValue::String(s1), PropertyValue::String(s2)) = (&prop1, &prop2) {
            assert!(Arc::ptr_eq(s1, s2), "Arc should be shared");
        }
    }

    // ========== Vector tests ==========

    #[test]
    fn test_vector_constructor() {
        let data = [1.0f32, 2.0, 3.0, 4.0];
        let vec_prop = PropertyValue::vector(data);

        assert_eq!(vec_prop.as_vector(), Some(&data[..]));
        assert_eq!(vec_prop.type_name(), "vector");
    }

    #[test]
    fn test_vector_from_vec() {
        let data: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let vec_prop: PropertyValue = data.clone().into();

        assert_eq!(vec_prop.as_vector(), Some(&data[..]));
    }

    #[test]
    fn test_vector_from_slice() {
        let data = [1.5f32, 2.5, 3.5];
        let vec_prop: PropertyValue = (&data[..]).into();

        assert_eq!(vec_prop.as_vector(), Some(&data[..]));
    }

    #[test]
    fn test_vector_display() {
        let vec_prop = PropertyValue::vector([1.0f32, 2.0, 3.0]);
        assert_eq!(format!("{}", vec_prop), "<vector[3]>");

        // Test with common embedding dimensions
        let embedding_384 = vec![0.0f32; 384];
        let vec_prop = PropertyValue::vector(&embedding_384);
        assert_eq!(format!("{}", vec_prop), "<vector[384]>");

        let embedding_1536 = vec![0.0f32; 1536];
        let vec_prop = PropertyValue::vector(&embedding_1536);
        assert_eq!(format!("{}", vec_prop), "<vector[1536]>");
    }

    #[test]
    fn test_vector_arc_sharing() {
        // Create a large vector (typical embedding size)
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 * 0.001).collect();
        let prop1 = PropertyValue::vector(&embedding);
        let prop2 = prop1.clone();

        // Both should point to the same Arc (cheap clone)
        if let (PropertyValue::Vector(v1), PropertyValue::Vector(v2)) = (&prop1, &prop2) {
            assert!(
                Arc::ptr_eq(v1, v2),
                "Vector Arc should be shared after clone"
            );
        }

        // Values should be equal
        assert_eq!(prop1, prop2);
        assert_eq!(prop1.as_vector(), prop2.as_vector());
    }

    #[test]
    fn test_vector_empty() {
        let empty: Vec<f32> = vec![];
        let vec_prop = PropertyValue::vector(&empty);

        assert_eq!(vec_prop.as_vector(), Some(&[][..]));
        assert_eq!(format!("{}", vec_prop), "<vector[0]>");
    }

    #[test]
    fn test_vector_accessor_wrong_type() {
        // as_vector should return None for non-vector types
        assert_eq!(PropertyValue::Null.as_vector(), None);
        assert_eq!(PropertyValue::Bool(true).as_vector(), None);
        assert_eq!(PropertyValue::Int(42).as_vector(), None);
        assert_eq!(PropertyValue::Float(1.5).as_vector(), None);
        assert_eq!(PropertyValue::string("hello").as_vector(), None);
        assert_eq!(PropertyValue::bytes([1, 2, 3]).as_vector(), None);
        assert_eq!(
            PropertyValue::array(vec![PropertyValue::Int(1)]).as_vector(),
            None
        );
    }

    #[test]
    fn test_vector_equality() {
        let v1 = PropertyValue::vector([1.0f32, 2.0, 3.0]);
        let v2 = PropertyValue::vector([1.0f32, 2.0, 3.0]);
        let v3 = PropertyValue::vector([1.0f32, 2.0, 4.0]);

        assert_eq!(v1, v2);
        assert_ne!(v1, v3);
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

    // ========== Serialization Tests ==========

    #[test]
    fn test_serialize_null() {
        let value = PropertyValue::Null;
        let bytes = value.serialize();
        assert_eq!(bytes, vec![TAG_NULL]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn test_serialize_bool() {
        // Test true
        let value = PropertyValue::Bool(true);
        let bytes = value.serialize();
        assert_eq!(bytes, vec![TAG_BOOL, 1]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 2);

        // Test false
        let value = PropertyValue::Bool(false);
        let bytes = value.serialize();
        assert_eq!(bytes, vec![TAG_BOOL, 0]);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn test_serialize_int() {
        let test_values = [0i64, 1, -1, i64::MAX, i64::MIN, 42, -12345];
        for &v in &test_values {
            let value = PropertyValue::Int(v);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_INT);
            assert_eq!(bytes.len(), 9);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, 9);
        }
    }

    #[test]
    fn test_serialize_float() {
        let test_values = [0.0f64, 1.0, -1.0, f64::MAX, f64::MIN, 3.14159, -2.71828];
        for &v in &test_values {
            let value = PropertyValue::Float(v);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_FLOAT);
            assert_eq!(bytes.len(), 9);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, 9);
        }
    }

    #[test]
    fn test_serialize_float_special_values() {
        // Test infinity
        let value = PropertyValue::Float(f64::INFINITY);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.as_float(), Some(f64::INFINITY));

        // Test negative infinity
        let value = PropertyValue::Float(f64::NEG_INFINITY);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized.as_float(), Some(f64::NEG_INFINITY));

        // Test NaN - special case, NaN != NaN
        let value = PropertyValue::Float(f64::NAN);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert!(deserialized.as_float().unwrap().is_nan());
    }

    #[test]
    fn test_serialize_string() {
        let test_values = ["", "hello", "world", "hello world!", "こんにちは", "🎉"];
        for s in test_values {
            let value = PropertyValue::string(s);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_STRING);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn test_serialize_bytes() {
        let test_values: &[&[u8]] = &[&[], &[1], &[1, 2, 3], &[0, 255, 128]];
        for &b in test_values {
            let value = PropertyValue::bytes(b);
            let bytes = value.serialize();

            assert_eq!(bytes[0], TAG_BYTES);

            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(deserialized, value);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn test_serialize_array() {
        // Empty array
        let value = PropertyValue::array(vec![]);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);

        // Array with mixed types
        let value = PropertyValue::array(vec![
            PropertyValue::Int(42),
            PropertyValue::string("hello"),
            PropertyValue::Bool(true),
            PropertyValue::Float(3.14),
        ]);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);

        // Nested array
        let inner = PropertyValue::array(vec![PropertyValue::Int(1), PropertyValue::Int(2)]);
        let value = PropertyValue::array(vec![inner, PropertyValue::Int(3)]);
        let bytes = value.serialize();
        let (deserialized, _) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
    }

    #[test]
    fn test_serialize_vector_basic() {
        let data = [1.0f32, 2.0, 3.0];
        let value = PropertyValue::vector(data);
        let bytes = value.serialize();

        // Check format: tag (1) + dimension (4) + 3*4 bytes
        assert_eq!(bytes[0], TAG_VECTOR);
        assert_eq!(bytes.len(), 1 + 4 + 3 * 4);

        let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
        assert_eq!(deserialized, value);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn test_serialize_vector_round_trip() {
        let data: Vec<f32> = (0..100).map(|i| i as f32 * 0.01).collect();
        let bytes = serialize_vector(&data);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert_eq!(deserialized.as_ref(), &data[..]);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn test_serialize_vector_empty() {
        let empty: Vec<f32> = vec![];
        let bytes = serialize_vector(&empty);

        // Should be tag (1) + dimension (4) = 5 bytes
        assert_eq!(bytes.len(), 5);
        assert_eq!(bytes[0], TAG_VECTOR);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert!(deserialized.is_empty());
        assert_eq!(consumed, 5);
    }

    #[test]
    fn test_serialize_vector_large() {
        // Test with typical embedding size (1536 dimensions like OpenAI ada-002)
        let large_vector: Vec<f32> = (0..1536).map(|i| (i as f32) / 1536.0).collect();
        let bytes = serialize_vector(&large_vector);

        // Expected size: tag (1) + dimension (4) + 1536*4 = 6149 bytes
        assert_eq!(bytes.len(), 1 + 4 + 1536 * 4);

        let (deserialized, consumed) = deserialize_vector(&bytes).unwrap();
        assert_eq!(deserialized.len(), 1536);
        assert_eq!(consumed, bytes.len());

        // Verify values
        for (i, &val) in deserialized.iter().enumerate() {
            assert!((val - (i as f32) / 1536.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_serialize_vector_special_values() {
        let data = [f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0, f32::NAN];
        let bytes = serialize_vector(&data);
        let (deserialized, _) = deserialize_vector(&bytes).unwrap();

        assert_eq!(deserialized[0], f32::INFINITY);
        assert_eq!(deserialized[1], f32::NEG_INFINITY);
        assert_eq!(deserialized[2], 0.0);
        assert_eq!(deserialized[3], 0.0); // -0.0 compares equal to 0.0
        assert!(deserialized[4].is_nan());
    }

    #[test]
    fn test_deserialize_vector_errors() {
        // Empty buffer
        let result = deserialize_vector(&[]);
        assert!(result.is_err());

        // Buffer too short for header
        let result = deserialize_vector(&[TAG_VECTOR, 1, 0, 0]);
        assert!(result.is_err());

        // Wrong type tag
        let result = deserialize_vector(&[TAG_INT, 3, 0, 0, 0]);
        assert!(result.is_err());

        // Buffer too short for data
        let mut bytes = vec![TAG_VECTOR, 3, 0, 0, 0]; // Dimension = 3
        bytes.extend_from_slice(&[1.0f32.to_le_bytes()[0]]); // Only 1 byte of data
        let result = deserialize_vector(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_property_value_errors() {
        // Empty buffer
        let result = PropertyValue::deserialize(&[]);
        assert!(result.is_err());

        // Unknown type tag
        let result = PropertyValue::deserialize(&[255]);
        assert!(result.is_err());

        // Truncated Int
        let result = PropertyValue::deserialize(&[TAG_INT, 1, 2, 3]);
        assert!(result.is_err());

        // Truncated String (length says 100, but no data)
        let result = PropertyValue::deserialize(&[TAG_STRING, 100, 0, 0, 0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_property_map_empty() {
        let map = PropertyMap::new();
        let bytes = map.serialize();

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

        let bytes = map.serialize();
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

        let bytes = map.serialize();
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

        let bytes = map.serialize();
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

    #[test]
    fn test_serialize_into_efficiency() {
        // Test that serialize_into appends to existing buffer correctly
        let mut buffer = vec![0xAA, 0xBB]; // Some existing data
        let value = PropertyValue::Int(42);
        value.serialize_into(&mut buffer);

        assert_eq!(buffer[0], 0xAA);
        assert_eq!(buffer[1], 0xBB);
        assert_eq!(buffer[2], TAG_INT);
        // The rest should be 42i64 in little-endian
    }

    #[test]
    fn test_all_property_types_round_trip() {
        // Comprehensive test of all PropertyValue types
        let values = vec![
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Bool(false),
            PropertyValue::Int(0),
            PropertyValue::Int(i64::MAX),
            PropertyValue::Int(i64::MIN),
            PropertyValue::Float(0.0),
            PropertyValue::Float(f64::MAX),
            PropertyValue::String(Arc::from("hello")),
            PropertyValue::String(Arc::from("")),
            PropertyValue::Bytes(Arc::from([1u8, 2, 3].as_slice())),
            PropertyValue::Bytes(Arc::from([].as_slice())),
            PropertyValue::Array(Arc::new(vec![
                PropertyValue::Int(1),
                PropertyValue::string("two"),
            ])),
            PropertyValue::Array(Arc::new(vec![])),
            PropertyValue::Vector(Arc::from([1.0f32, 2.0, 3.0].as_slice())),
            PropertyValue::Vector(Arc::from([].as_slice())),
        ];

        for value in values {
            let bytes = value.serialize();
            let (deserialized, consumed) = PropertyValue::deserialize(&bytes).unwrap();
            assert_eq!(
                consumed,
                bytes.len(),
                "Consumed bytes should match serialized length for {:?}",
                value.type_name()
            );

            // Special handling for NaN values
            if let PropertyValue::Float(f) = &value {
                if f.is_nan() {
                    assert!(deserialized.as_float().unwrap().is_nan());
                    continue;
                }
            }
            assert_eq!(
                deserialized,
                value,
                "Round-trip failed for {:?}",
                value.type_name()
            );
        }
    }

    #[test]
    fn test_endianness() {
        // Verify little-endian serialization
        let value = PropertyValue::Int(0x0102030405060708i64);
        let bytes = value.serialize();

        // Little-endian: least significant byte first
        assert_eq!(bytes[0], TAG_INT);
        assert_eq!(bytes[1], 0x08);
        assert_eq!(bytes[2], 0x07);
        assert_eq!(bytes[3], 0x06);
        assert_eq!(bytes[4], 0x05);
        assert_eq!(bytes[5], 0x04);
        assert_eq!(bytes[6], 0x03);
        assert_eq!(bytes[7], 0x02);
        assert_eq!(bytes[8], 0x01);
    }
}
