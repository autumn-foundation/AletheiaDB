//! Type conversion utilities for the HTTP API.
//!
//! This module provides functions to convert between AletheiaDB's internal types
//! (like [`PropertyMap`] and [`PropertyValue`]) and JSON values used in the HTTP API.
//!
//! # Safety Limits
//!
//! To prevent stack overflow attacks via deeply nested JSON structures, recursive
//! conversion functions enforce a maximum recursion depth of 100 levels.

use crate::core::{GLOBAL_INTERNER, PropertyMap, PropertyMapBuilder, PropertyValue};
use crate::query::converter::ParameterValue;
use crate::query::executor::{EntityId, EntityResult, QueryRow};
use crate::query::ir::PredicateValue;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum recursion depth for JSON processing to prevent stack overflow.
const MAX_JSON_RECURSION_DEPTH: usize = 100;

/// Resolves an interned string to its string representation.
///
/// # Arguments
///
/// * `interned` - The interned string handle to resolve.
///
/// # Returns
///
/// The string value if resolution succeeds, or a placeholder `<unknown:ID>` if
/// the interned string cannot be found (which should not happen in normal operation).
pub fn interned_to_string(interned: crate::core::InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(interned, |s| s.to_string())
        .unwrap_or_else(|| format!("<unknown:{}>", interned.as_u32()))
}

/// Converts a [`PropertyMap`] to a JSON object (HashMap).
///
/// Keys are resolved from interned strings to standard strings.
/// Values are recursively converted using [`property_value_to_json`].
///
/// # Arguments
///
/// * `props` - The property map to convert.
///
/// # Returns
///
/// * `Ok(HashMap)` - The JSON object representation.
/// * `Err(String)` - If a value conversion fails (e.g., recursion depth exceeded).
pub fn property_map_to_json(
    props: &PropertyMap,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let mut result = HashMap::new();
    for (key, value) in props.iter() {
        let key_str = interned_to_string(*key);
        result.insert(key_str, property_value_to_json(value)?);
    }
    Ok(result)
}

/// Converts a [`PropertyValue`] to a serde JSON Value.
///
/// # Mappings
///
/// | AletheiaDB Type | JSON Type | Notes |
/// |------------------|-----------|-------|
/// | `Null` | `null` | |
/// | `Bool` | `boolean` | |
/// | `Int` | `number` | |
/// | `Float` | `number` | |
/// | `String` | `string` | |
/// | `Bytes` | `array` | Array of integers (byte values) |
/// | `Array` | `array` | Recursive conversion |
/// | `Vector` | `array` | Array of floats |
/// | `SparseVector` | `object` | `{"indices": [...], "values": [...]}` |
///
/// # Errors
///
/// Returns an error if the structure is too deeply nested (exceeds 100 levels).
pub fn property_value_to_json(value: &PropertyValue) -> Result<serde_json::Value, String> {
    property_value_to_json_recursive(value, 0)
}

fn property_value_to_json_recursive(
    value: &PropertyValue,
    depth: usize,
) -> Result<serde_json::Value, String> {
    if depth >= MAX_JSON_RECURSION_DEPTH {
        return Err(format!(
            "Recursion limit exceeded (max {})",
            MAX_JSON_RECURSION_DEPTH
        ));
    }

    match value {
        PropertyValue::Null => Ok(serde_json::Value::Null),
        PropertyValue::Bool(b) => Ok(serde_json::Value::Bool(*b)),
        PropertyValue::Int(i) => Ok(json!(*i)),
        PropertyValue::Float(f) => Ok(json!(*f)),
        PropertyValue::String(s) => Ok(serde_json::Value::String(s.to_string())),
        PropertyValue::Bytes(b) => {
            // Encode bytes as array of integers since base64 is not in http-server features
            Ok(serde_json::Value::Array(
                b.iter().map(|byte| json!(*byte)).collect(),
            ))
        }
        PropertyValue::Array(arr) => {
            let items: Result<Vec<_>, String> = arr
                .iter()
                .map(|v| property_value_to_json_recursive(v, depth + 1))
                .collect();
            Ok(serde_json::Value::Array(items?))
        }
        PropertyValue::Vector(v) => Ok(serde_json::Value::Array(
            v.iter().map(|f| json!(*f)).collect(),
        )),
        PropertyValue::SparseVector(sv) => Ok(json!({
            "indices": sv.indices(),
            "values": sv.values()
        })),
    }
}

/// Converts a JSON object to a [`PropertyMap`].
///
/// # Arguments
///
/// * `json` - The JSON object (HashMap) to convert.
///
/// # Returns
///
/// * `Ok(PropertyMap)` - The converted property map.
/// * `Err(String)` - If conversion fails (e.g., unsupported types or recursion limit).
pub fn json_to_property_map(
    json: &HashMap<String, serde_json::Value>,
) -> Result<PropertyMap, String> {
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in json {
        let pv = json_to_property_value(value)?;
        builder = builder
            .try_insert(key.as_str(), pv)
            .map_err(|e| e.to_string())?;
    }
    Ok(builder.build())
}

/// Converts a JSON parameter map to AletheiaDB parameter values.
pub fn json_to_parameter_map(
    json: &HashMap<String, serde_json::Value>,
) -> Result<HashMap<String, ParameterValue>, String> {
    let mut params = HashMap::new();
    for (key, value) in json {
        params.insert(key.clone(), json_to_parameter_value(value)?);
    }
    Ok(params)
}

/// Converts a JSON value to a ParameterValue.
pub fn json_to_parameter_value(value: &serde_json::Value) -> Result<ParameterValue, String> {
    match value {
        serde_json::Value::Null => Ok(ParameterValue::Value(PredicateValue::Null)),
        serde_json::Value::Bool(b) => Ok(ParameterValue::Value(PredicateValue::Bool(*b))),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(ParameterValue::Value(PredicateValue::Int(i)))
            } else {
                let f = n
                    .as_f64()
                    .ok_or_else(|| "Invalid number format".to_string())?;
                Ok(ParameterValue::Value(PredicateValue::Float(f)))
            }
        }
        serde_json::Value::String(s) => {
            Ok(ParameterValue::Value(PredicateValue::String(s.clone())))
        }
        serde_json::Value::Array(arr) => {
            // Check if it's an embedding (vector of floats)
            let floats: Result<Vec<f32>, String> = arr
                .iter()
                .map(|v| {
                    v.as_f64()
                        .map(|f| f as f32)
                        .ok_or_else(|| "Invalid float in embedding array".to_string())
                })
                .collect();

            match floats {
                Ok(f) => Ok(ParameterValue::Embedding(Arc::from(f))),
                Err(e) => Err(e),
            }
        }
        serde_json::Value::Object(_) => {
            Err("Objects are not supported as parameter values".to_string())
        }
    }
}

/// Converts a QueryRow to a JSON object.
pub fn query_row_to_json(row: QueryRow) -> Result<serde_json::Value, String> {
    let mut obj = serde_json::Map::new();

    // Add entity data
    match row.entity {
        EntityResult::Node(node) => {
            obj.insert(
                "node".to_string(),
                json!({
                    "id": node.id.as_u64(),
                    "label": interned_to_string(node.label),
                    "properties": property_map_to_json(&node.properties)?
                }),
            );
        }
        EntityResult::Edge(edge) => {
            obj.insert(
                "edge".to_string(),
                json!({
                    "id": edge.id.as_u64(),
                    "label": interned_to_string(edge.label),
                    "source": edge.source.as_u64(),
                    "target": edge.target.as_u64(),
                    "properties": property_map_to_json(&edge.properties)?
                }),
            );
        }
        EntityResult::NodeId(id) => {
            obj.insert("node_id".to_string(), json!(id.as_u64()));
        }
        EntityResult::EdgeId(id) => {
            obj.insert("edge_id".to_string(), json!(id.as_u64()));
        }
    }

    // Add metadata
    if let Some(score) = row.score {
        obj.insert("score".to_string(), json!(score));
    }

    if let Some(path) = row.path {
        let path_json: Vec<serde_json::Value> = path
            .iter()
            .map(|id| match id {
                EntityId::Node(nid) => json!({"type": "node", "id": nid.as_u64()}),
                EntityId::Edge(eid) => json!({"type": "edge", "id": eid.as_u64()}),
            })
            .collect();
        obj.insert("path".to_string(), serde_json::Value::Array(path_json));
    }

    if let Some(ts) = row.timestamp {
        obj.insert("timestamp".to_string(), json!(ts.wallclock()));
    }

    Ok(serde_json::Value::Object(obj))
}

/// Converts a serde JSON Value to a [`PropertyValue`].
///
/// # Mappings
///
/// | JSON Type | AletheiaDB Type | Notes |
/// |-----------|------------------|-------|
/// | `null` | `Null` | |
/// | `boolean` | `Bool` | |
/// | `number` (int) | `Int` | |
/// | `number` (float)| `Float` | |
/// | `string` | `String` | |
/// | `array` | `Vector` | If array contains only numbers |
/// | `array` | `Array` | Mixed types or nested arrays |
/// | `object` | N/A | Not supported (returns error) |
///
/// # Heuristics
///
/// When encountering a JSON array, the converter attempts to interpret it as a
/// numeric vector (`PropertyValue::Vector`) if all elements are numbers. If
/// any element is not a number, it falls back to a generic array (`PropertyValue::Array`).
///
/// # Errors
///
/// Returns an error if:
/// - The structure is too deeply nested (exceeds 100 levels).
/// - The JSON contains objects (nested objects are not supported as property values).
pub fn json_to_property_value(value: &serde_json::Value) -> Result<PropertyValue, String> {
    json_to_property_value_recursive(value, 0)
}

fn json_to_property_value_recursive(
    value: &serde_json::Value,
    depth: usize,
) -> Result<PropertyValue, String> {
    // Changed > to >= for strict limit adherence
    if depth >= MAX_JSON_RECURSION_DEPTH {
        return Err(format!(
            "Recursion limit exceeded (max {})",
            MAX_JSON_RECURSION_DEPTH
        ));
    }

    match value {
        serde_json::Value::Null => Ok(PropertyValue::Null),
        serde_json::Value::Bool(b) => Ok(PropertyValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(PropertyValue::Int(i))
            } else {
                n.as_f64()
                    .map(PropertyValue::Float)
                    .ok_or_else(|| "Invalid number format".to_string())
            }
        }
        serde_json::Value::String(s) => Ok(PropertyValue::String(Arc::from(s.as_str()))),
        serde_json::Value::Array(arr) => {
            // Early check for array size limit before any processing
            if arr.len() > crate::core::property::MAX_ARRAY_ELEMENTS {
                return Err(format!(
                    "Array count {} exceeds maximum allowed {}",
                    arr.len(),
                    crate::core::property::MAX_ARRAY_ELEMENTS
                ));
            }

            if arr.iter().all(|v| v.is_number()) && !arr.is_empty() {
                // Early check for vector dimension limit before allocation
                if arr.len() > crate::core::property::MAX_VECTOR_DIMENSIONS {
                    return Err(format!(
                        "Vector dimension {} exceeds limit {}",
                        arr.len(),
                        crate::core::property::MAX_VECTOR_DIMENSIONS
                    ));
                }

                let floats: Result<Vec<f32>, String> = arr
                    .iter()
                    .map(|v| {
                        v.as_f64()
                            .map(|f| f as f32)
                            .ok_or_else(|| "Invalid float in array".to_string())
                    })
                    .collect();

                if let Ok(floats) = floats {
                    return Ok(PropertyValue::Vector(Arc::from(floats)));
                }
            }

            let values: Result<Vec<PropertyValue>, String> = arr
                .iter()
                .map(|v| json_to_property_value_recursive(v, depth + 1))
                .collect();
            Ok(PropertyValue::Array(Arc::new(values?)))
        }
        serde_json::Value::Object(_) => {
            Err("Nested objects are not supported as property values".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_json_recursion_limit() {
        // Create deeply nested JSON: [[[[...]]]]
        let mut value = json!(1);
        let depth = 200;
        for _ in 0..depth {
            value = json!([value]);
        }

        let result = json_to_property_value(&value);

        match result {
            Ok(_) => panic!("Recursion limit was not enforced!"),
            Err(e) => assert!(
                e.contains("Recursion limit exceeded"),
                "Unexpected error: {}",
                e
            ),
        }
    }

    #[test]
    fn test_json_recursion_boundary() {
        // Depth 99 should succeed (depth 0 is root, so 0..99 is 100 levels)
        // Use "null" to avoid vector optimization shortcut
        let mut value_99 = json!(null);

        // Creating nesting level 99
        for _ in 0..99 {
            value_99 = json!([value_99]);
        }
        assert!(
            json_to_property_value(&value_99).is_ok(),
            "Depth 99 should pass"
        );

        // Creating nesting level 100
        let mut value_100 = json!(null);
        for _ in 0..100 {
            value_100 = json!([value_100]);
        }
        let res = json_to_property_value(&value_100);
        assert!(res.is_err(), "Depth 100 should fail");
        assert!(res.unwrap_err().contains("Recursion limit exceeded"));
    }

    #[test]
    fn test_property_value_to_json_recursion_limit() {
        // Create deeply nested PropertyValue
        let mut val = PropertyValue::Int(1);
        let depth = 200;
        for _ in 0..depth {
            val = PropertyValue::Array(Arc::new(vec![val]));
        }

        let result = property_value_to_json(&val);
        match result {
            Ok(_) => panic!("Recursion limit was not enforced for serialization!"),
            Err(e) => assert!(
                e.contains("Recursion limit exceeded"),
                "Unexpected error: {}",
                e
            ),
        }
    }

    #[test]
    fn test_json_vector_dimension_bypass() {
        use crate::core::property::MAX_VECTOR_DIMENSIONS;

        // Create a JSON array that exceeds MAX_VECTOR_DIMENSIONS
        // We use a small overflow to verify boundary condition
        let too_large = MAX_VECTOR_DIMENSIONS + 1;

        // Construct a large vector of numbers
        // Note: generating this large structure in memory is acceptable for a test
        let large_vec: Vec<serde_json::Value> =
            std::iter::repeat_n(json!(1.0), too_large).collect();
        let json_val = serde_json::Value::Array(large_vec);

        let result = json_to_property_value(&json_val);

        // This should now fail with a dimension limit error
        match result {
            Ok(_) => panic!("Validation failed: should have rejected large vector"),
            Err(e) => {
                assert!(
                    e.contains("exceeds limit"),
                    "Unexpected error message: {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_json_array_limit_enforced() {
        use crate::core::property::MAX_ARRAY_ELEMENTS;

        // Create array slightly over limit
        // We use nulls to ensure fallback to PropertyValue::Array (generic array)
        // instead of PropertyValue::Vector (numeric vector)
        let limit = MAX_ARRAY_ELEMENTS + 1;
        let vec: Vec<serde_json::Value> = std::iter::repeat_n(json!(null), limit).collect();
        let val = serde_json::Value::Array(vec);

        let res = json_to_property_value(&val);
        assert!(res.is_err(), "Should enforce MAX_ARRAY_ELEMENTS");
        assert!(res.unwrap_err().contains("exceeds maximum allowed"));
    }

    #[test]
    fn test_json_vector_pre_allocation_limit() {
        use crate::core::property::MAX_ARRAY_ELEMENTS;
        use crate::core::property::MAX_VECTOR_DIMENSIONS;

        // Verify that numeric vectors are checked BEFORE allocation
        // This test relies on the fact that MAX_VECTOR_DIMENSIONS < MAX_ARRAY_ELEMENTS
        const { assert!(MAX_VECTOR_DIMENSIONS < MAX_ARRAY_ELEMENTS) };

        // Create a numeric vector that is larger than MAX_VECTOR_DIMENSIONS but smaller than MAX_ARRAY_ELEMENTS
        // This should fail with "Vector dimension X exceeds limit"
        let size = MAX_VECTOR_DIMENSIONS + 100;
        let vec: Vec<serde_json::Value> = std::iter::repeat_n(json!(1.0), size).collect();
        let val = serde_json::Value::Array(vec);

        let res = json_to_property_value(&val);
        assert!(res.is_err());
        // Should hit vector limit, not array limit
        assert!(res.unwrap_err().contains("Vector dimension"));
    }
}
