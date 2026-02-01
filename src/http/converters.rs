use crate::core::{GLOBAL_INTERNER, PropertyMap, PropertyMapBuilder, PropertyValue};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum recursion depth for JSON processing to prevent stack overflow.
const MAX_JSON_RECURSION_DEPTH: usize = 100;

pub fn interned_to_string(interned: crate::core::InternedString) -> String {
    GLOBAL_INTERNER
        .resolve(interned)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("<unknown:{}>", interned.as_u32()))
}

pub fn property_map_to_json(props: &PropertyMap) -> HashMap<String, serde_json::Value> {
    let mut result = HashMap::new();
    for (key, value) in props.iter() {
        let key_str = interned_to_string(*key);
        result.insert(key_str, property_value_to_json(value));
    }
    result
}

pub fn property_value_to_json(value: &PropertyValue) -> serde_json::Value {
    match value {
        PropertyValue::Null => serde_json::Value::Null,
        PropertyValue::Bool(b) => serde_json::Value::Bool(*b),
        PropertyValue::Int(i) => json!(*i),
        PropertyValue::Float(f) => json!(*f),
        PropertyValue::String(s) => serde_json::Value::String(s.to_string()),
        PropertyValue::Bytes(b) => {
            // Encode bytes as array of integers since base64 is not in http-server features
            serde_json::Value::Array(b.iter().map(|byte| json!(*byte)).collect())
        }
        PropertyValue::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(property_value_to_json).collect())
        }
        PropertyValue::Vector(v) => serde_json::Value::Array(v.iter().map(|f| json!(*f)).collect()),
        PropertyValue::SparseVector(sv) => {
            json!({
                "indices": sv.indices(),
                "values": sv.values()
            })
        }
    }
}

pub fn json_to_property_map(
    json: &HashMap<String, serde_json::Value>,
) -> Result<PropertyMap, String> {
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in json {
        let pv = json_to_property_value(value)?;
        builder = builder.insert(key.as_str(), pv);
    }
    Ok(builder.build())
}

pub fn json_to_property_value(value: &serde_json::Value) -> Result<PropertyValue, String> {
    json_to_property_value_recursive(value, 0)
}

fn json_to_property_value_recursive(
    value: &serde_json::Value,
    depth: usize,
) -> Result<PropertyValue, String> {
    if depth > MAX_JSON_RECURSION_DEPTH {
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
            if arr.iter().all(|v| v.is_number()) && !arr.is_empty() {
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
}
