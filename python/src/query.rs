//! Cypher / AQL query execution bindings.

use crate::errors::map_error;
use crate::types::{PyEdge, PyNode};
use aletheiadb::AletheiaDB as RustDB;
use aletheiadb::cypher::CypherParameterValue;
use aletheiadb::query::executor::EntityResult;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};
use std::collections::HashMap;
use std::sync::Arc;

fn py_to_cypher_param(value: &Bound<'_, PyAny>) -> PyResult<CypherParameterValue> {
    if value.is_none() {
        return Ok(CypherParameterValue::Null);
    }
    if let Ok(b) = value.downcast::<PyBool>() {
        return Ok(CypherParameterValue::Bool(b.is_true()));
    }
    if value.is_instance_of::<PyInt>() {
        return Ok(CypherParameterValue::Int(value.extract()?));
    }
    if value.is_instance_of::<PyFloat>() {
        return Ok(CypherParameterValue::Float(value.extract()?));
    }
    if let Ok(s) = value.downcast::<PyString>() {
        let owned: String = s.extract()?;
        return Ok(CypherParameterValue::String(owned));
    }
    if let Ok(list) = value.downcast::<PyList>() {
        let mut buf: Vec<f32> = Vec::with_capacity(list.len());
        for item in list.iter() {
            let f: f64 = item
                .extract()
                .map_err(|_| PyTypeError::new_err("Embedding parameters must be lists of numbers"))?;
            buf.push(f as f32);
        }
        return Ok(CypherParameterValue::Embedding(Arc::from(buf)));
    }
    Err(PyTypeError::new_err(format!(
        "Unsupported Cypher parameter type: {}",
        value.get_type().name()?
    )))
}

pub fn execute_cypher(
    py: Python<'_>,
    db: &Arc<RustDB>,
    query: &str,
    params: Option<&Bound<'_, PyDict>>,
) -> PyResult<Py<PyList>> {
    let mut param_map: HashMap<String, CypherParameterValue> = HashMap::new();
    if let Some(p) = params {
        for (k, v) in p.iter() {
            let key: String = k
                .extract()
                .map_err(|_| PyValueError::new_err("Cypher parameter names must be strings"))?;
            param_map.insert(key, py_to_cypher_param(&v)?);
        }
    }
    let query_owned = query.to_owned();
    let db_clone = Arc::clone(db);
    let rows = py
        .allow_threads(move || {
            let results = if param_map.is_empty() {
                db_clone.execute_cypher(&query_owned)
            } else {
                db_clone.execute_cypher_with_params(&query_owned, param_map)
            }?;
            results.collect_all()
        })
        .map_err(map_error)?;

    let list = PyList::empty_bound(py);
    for row in rows {
        let d = PyDict::new_bound(py);
        match row.entity {
            EntityResult::Node(n) => {
                d.set_item("kind", "node")?;
                d.set_item("entity", Py::new(py, PyNode::from_rust(n))?)?;
            }
            EntityResult::Edge(e) => {
                d.set_item("kind", "edge")?;
                d.set_item("entity", Py::new(py, PyEdge::from_rust(e))?)?;
            }
            EntityResult::NodeId(id) => {
                d.set_item("kind", "node_id")?;
                d.set_item("entity", id.as_u64())?;
            }
            EntityResult::EdgeId(id) => {
                d.set_item("kind", "edge_id")?;
                d.set_item("entity", id.as_u64())?;
            }
            // Null binding from an unmatched OPTIONAL MATCH pattern (left-outer
            // semantics): the row is preserved but carries no entity.
            EntityResult::Null => {
                d.set_item("kind", "null")?;
                d.set_item("entity", py.None())?;
            }
            // EntityResult is #[non_exhaustive]; surface future variants
            // rather than failing the whole result set.
            _ => {
                d.set_item("kind", "unknown")?;
                d.set_item("entity", py.None())?;
            }
        }
        if let Some(score) = row.score {
            d.set_item("score", score as f64)?;
        }
        list.append(d)?;
    }
    Ok(list.unbind())
}

/// Execute an AQL query (`MATCH ... RETURN ...`) and return rows.
#[pyfunction]
pub fn execute_aql(py: Python<'_>, db: &crate::db::PyAletheiaDB, query: &str) -> PyResult<Py<PyList>> {
    let db_clone = db.inner();
    let query_owned = query.to_owned();
    let rows = py
        .allow_threads(move || db_clone.execute_aql(&query_owned).and_then(|r| r.collect_all()))
        .map_err(map_error)?;
    let list = PyList::empty_bound(py);
    for row in rows {
        let d = PyDict::new_bound(py);
        match row.entity {
            EntityResult::Node(n) => {
                d.set_item("kind", "node")?;
                d.set_item("entity", Py::new(py, PyNode::from_rust(n))?)?;
            }
            EntityResult::Edge(e) => {
                d.set_item("kind", "edge")?;
                d.set_item("entity", Py::new(py, PyEdge::from_rust(e))?)?;
            }
            EntityResult::NodeId(id) => {
                d.set_item("kind", "node_id")?;
                d.set_item("entity", id.as_u64())?;
            }
            EntityResult::EdgeId(id) => {
                d.set_item("kind", "edge_id")?;
                d.set_item("entity", id.as_u64())?;
            }
            // Null binding from an unmatched OPTIONAL MATCH pattern (left-outer
            // semantics): the row is preserved but carries no entity.
            EntityResult::Null => {
                d.set_item("kind", "null")?;
                d.set_item("entity", py.None())?;
            }
            // EntityResult is #[non_exhaustive]; surface future variants
            // rather than failing the whole result set.
            _ => {
                d.set_item("kind", "unknown")?;
                d.set_item("entity", py.None())?;
            }
        }
        if let Some(score) = row.score {
            d.set_item("score", score as f64)?;
        }
        list.append(d)?;
    }
    Ok(list.unbind())
}
