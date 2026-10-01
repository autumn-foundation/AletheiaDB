//! Python <-> AletheiaDB value conversions and wrapper classes.

use aletheiadb::core::{
    Edge as RustEdge, GLOBAL_INTERNER, InternedString, Node as RustNode, PropertyMap,
    PropertyMapBuilder, PropertyValue, Timestamp,
};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString};

/// Resolve an interned string to an owned `String`.
pub fn resolve_interned(s: InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(s, |st| st.to_string())
        .unwrap_or_default()
}

/// Convert a Python object into a `PropertyValue`.
///
/// Mapping:
/// - bool -> Bool
/// - int -> Int
/// - float -> Float
/// - str -> String
/// - bytes -> Bytes
/// - list of float -> Vector (if all numeric), else Array
/// - list (mixed) -> Array
/// - None -> Null
pub fn py_to_property_value(value: &Bound<'_, PyAny>) -> PyResult<PropertyValue> {
    if value.is_none() {
        return Ok(PropertyValue::Null);
    }
    if let Ok(b) = value.downcast::<PyBool>() {
        return Ok(PropertyValue::Bool(b.is_true()));
    }
    if let Ok(b) = value.downcast::<PyBytes>() {
        return Ok(PropertyValue::bytes(b.as_bytes()));
    }
    if value.is_instance_of::<PyInt>() {
        let i: i64 = value.extract()?;
        return Ok(PropertyValue::Int(i));
    }
    if value.is_instance_of::<PyFloat>() {
        let f: f64 = value.extract()?;
        return Ok(PropertyValue::Float(f));
    }
    if let Ok(s) = value.downcast::<PyString>() {
        let owned: String = s.extract()?;
        return Ok(PropertyValue::string(&owned));
    }
    if let Ok(list) = value.downcast::<PyList>() {
        let mut all_numeric = true;
        for item in list.iter() {
            if !(item.is_instance_of::<PyFloat>() || item.is_instance_of::<PyInt>()) {
                all_numeric = false;
                break;
            }
            if let Ok(b) = item.downcast::<PyBool>() {
                let _ = b;
                all_numeric = false;
                break;
            }
        }
        if all_numeric && !list.is_empty() {
            let mut vec_buf: Vec<f32> = Vec::with_capacity(list.len());
            for item in list.iter() {
                let v: f64 = item.extract()?;
                vec_buf.push(v as f32);
            }
            return Ok(PropertyValue::vector(&vec_buf));
        }
        let mut values = Vec::with_capacity(list.len());
        for item in list.iter() {
            values.push(py_to_property_value(&item)?);
        }
        return Ok(PropertyValue::array(values));
    }
    Err(PyTypeError::new_err(format!(
        "Unsupported property type: {}",
        value.get_type().name()?
    )))
}

/// Convert a `PropertyValue` to a Python object.
pub fn property_value_to_py(py: Python<'_>, value: &PropertyValue) -> PyResult<PyObject> {
    Ok(match value {
        PropertyValue::Null => py.None(),
        PropertyValue::Bool(b) => b.into_py(py),
        PropertyValue::Int(i) => i.into_py(py),
        PropertyValue::Float(f) => f.into_py(py),
        PropertyValue::String(s) => s.as_ref().into_py(py),
        PropertyValue::Bytes(b) => PyBytes::new_bound(py, b.as_ref()).into_py(py),
        PropertyValue::Array(arr) => {
            let list = PyList::empty_bound(py);
            for v in arr.iter() {
                list.append(property_value_to_py(py, v)?)?;
            }
            list.into_py(py)
        }
        PropertyValue::Vector(v) => {
            let list = PyList::empty_bound(py);
            for f in v.iter() {
                list.append(*f as f64)?;
            }
            list.into_py(py)
        }
        PropertyValue::SparseVector(sv) => {
            let d = PyDict::new_bound(py);
            let idx = PyList::empty_bound(py);
            let vals = PyList::empty_bound(py);
            for i in sv.indices() {
                idx.append(*i)?;
            }
            for v in sv.values() {
                vals.append(*v as f64)?;
            }
            d.set_item("indices", idx)?;
            d.set_item("values", vals)?;
            d.into_py(py)
        }
    })
}

/// Convert a Python dict (or None) into a `PropertyMap`.
pub fn py_dict_to_property_map(dict: Option<&Bound<'_, PyDict>>) -> PyResult<PropertyMap> {
    let Some(dict) = dict else {
        return Ok(PropertyMap::new());
    };
    let mut builder = PropertyMapBuilder::new();
    for (k, v) in dict.iter() {
        let key: String = k
            .extract()
            .map_err(|_| PyValueError::new_err("Property keys must be strings"))?;
        let value = py_to_property_value(&v)?;
        builder = builder.insert(&key, value);
    }
    Ok(builder.build())
}

/// Convert a `PropertyMap` to a Python dict.
pub fn property_map_to_py_dict(py: Python<'_>, map: &PropertyMap) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new_bound(py);
    for (key, value) in map.iter() {
        let key_str = resolve_interned(*key);
        // Elide engine-reserved ride-along keys (the namespace marker, #3349,
        // and crypto-shred markers): they are surfaced as first-class fields,
        // never as user properties.
        if aletheiadb::core::namespace::is_reserved_property_key(&key_str) {
            continue;
        }
        dict.set_item(key_str, property_value_to_py(py, value)?)?;
    }
    Ok(dict.unbind())
}

/// Python wrapper for a graph node.
#[pyclass(name = "Node", module = "aletheiadb._native", frozen)]
pub struct PyNode {
    #[pyo3(get)]
    pub id: u64,
    #[pyo3(get)]
    pub label: String,
    pub properties_map: PropertyMap,
}

#[pymethods]
impl PyNode {
    #[getter]
    fn properties(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        property_map_to_py_dict(py, &self.properties_map)
    }

    fn __repr__(&self) -> String {
        format!("Node(id={}, label={:?})", self.id, self.label)
    }
}

impl PyNode {
    pub fn from_rust(node: RustNode) -> Self {
        Self {
            id: node.id.as_u64(),
            label: resolve_interned(node.label),
            properties_map: node.properties,
        }
    }
}

/// Python wrapper for a graph edge.
#[pyclass(name = "Edge", module = "aletheiadb._native", frozen)]
pub struct PyEdge {
    #[pyo3(get)]
    pub id: u64,
    #[pyo3(get)]
    pub source_id: u64,
    #[pyo3(get)]
    pub target_id: u64,
    #[pyo3(get)]
    pub label: String,
    pub properties_map: PropertyMap,
}

#[pymethods]
impl PyEdge {
    #[getter]
    fn properties(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        property_map_to_py_dict(py, &self.properties_map)
    }

    fn __repr__(&self) -> String {
        format!(
            "Edge(id={}, source_id={}, target_id={}, label={:?})",
            self.id, self.source_id, self.target_id, self.label
        )
    }
}

impl PyEdge {
    pub fn from_rust(edge: RustEdge) -> Self {
        Self {
            id: edge.id.as_u64(),
            source_id: edge.source.as_u64(),
            target_id: edge.target.as_u64(),
            label: resolve_interned(edge.label),
            properties_map: edge.properties,
        }
    }
}

fn timestamp_from_micros(micros: i64) -> PyResult<Timestamp> {
    use aletheiadb::core::hlc::HybridTimestamp;
    HybridTimestamp::new(micros, 0)
        .map_err(|e| PyValueError::new_err(format!("Invalid timestamp: {}", e)))
}

/// Parse a Python datetime, ISO-8601 string, or integer (microseconds since epoch)
/// into an AletheiaDB Timestamp.
pub fn parse_timestamp(_py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Timestamp> {
    use aletheiadb::time;
    if value.is_none() {
        return Ok(time::now());
    }
    if value.is_instance_of::<PyInt>() {
        let micros: i64 = value.extract()?;
        return timestamp_from_micros(micros);
    }
    if let Ok(s) = value.downcast::<PyString>() {
        let owned: String = s.extract()?;
        let dt = chrono::DateTime::parse_from_rfc3339(&owned)
            .map_err(|e| PyValueError::new_err(format!("Invalid ISO-8601 timestamp {:?}: {}", owned, e)))?;
        return timestamp_from_micros(dt.timestamp_micros());
    }
    let ts: f64 = value
        .call_method0("timestamp")
        .and_then(|t| t.extract())
        .map_err(|_| PyTypeError::new_err("Expected datetime, ISO-8601 string, or int micros"))?;
    timestamp_from_micros((ts * 1_000_000.0) as i64)
}
