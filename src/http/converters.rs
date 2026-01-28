use crate::core::{GLOBAL_INTERNER, PropertyMap, PropertyMapBuilder, PropertyValue};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

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

pub fn json_to_property_map(json: &HashMap<String, serde_json::Value>) -> PropertyMap {
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in json {
        if let Some(pv) = json_to_property_value(value) {
            builder = builder.insert(key.as_str(), pv);
        }
    }
    builder.build()
}

pub fn json_to_property_value(value: &serde_json::Value) -> Option<PropertyValue> {
    match value {
        serde_json::Value::Null => Some(PropertyValue::Null),
        serde_json::Value::Bool(b) => Some(PropertyValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(PropertyValue::Int(i))
            } else {
                n.as_f64().map(PropertyValue::Float)
            }
        }
        serde_json::Value::String(s) => Some(PropertyValue::String(Arc::from(s.as_str()))),
        serde_json::Value::Array(arr) => {
            if arr.iter().all(|v| v.is_number()) && !arr.is_empty() {
                let floats: Vec<f32> = arr
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                if floats.len() == arr.len() {
                    return Some(PropertyValue::Vector(Arc::from(floats)));
                }
            }
            let values: Vec<PropertyValue> = arr.iter().filter_map(json_to_property_value).collect();
            Some(PropertyValue::Array(Arc::new(values)))
        }
        serde_json::Value::Object(_) => None,
    }
}
