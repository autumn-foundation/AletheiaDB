//! `AletheiaDB` Python wrapper.

use crate::errors::{ConfigErrorPy, map_error, map_storage_error};
use crate::types::{
    PyEdge, PyNode, parse_timestamp, property_map_to_py_dict, py_dict_to_property_map,
};
use aletheiadb::api::WriteOps;
use aletheiadb::core::{EdgeId, NodeId};
use aletheiadb::{AletheiaDB as RustDB, Error};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::sync::Arc;

fn node_id(id: u64) -> PyResult<NodeId> {
    NodeId::new(id).map_err(map_storage_error)
}

fn edge_id(id: u64) -> PyResult<EdgeId> {
    EdgeId::new(id).map_err(map_storage_error)
}

/// In-process AletheiaDB instance.
///
/// The underlying Rust `AletheiaDB` is `Send + Sync` (it uses internal
/// concurrent structures — DashMap, parking_lot locks, Arcs), so the Python
/// wrapper can be shared across threads.
#[pyclass(name = "AletheiaDB", module = "aletheiadb._native")]
pub struct PyAletheiaDB {
    pub(crate) inner: Arc<RustDB>,
}

impl PyAletheiaDB {
    pub fn inner(&self) -> Arc<RustDB> {
        Arc::clone(&self.inner)
    }
}

#[pymethods]
impl PyAletheiaDB {
    /// Create a new in-memory database.
    #[new]
    fn new() -> PyResult<Self> {
        let db = RustDB::new().map_err(map_error)?;
        Ok(Self { inner: Arc::new(db) })
    }

    /// Open a database from a TOML config file.
    #[staticmethod]
    fn open(config_path: &str) -> PyResult<Self> {
        use aletheiadb::AletheiaDBConfig;
        let config = AletheiaDBConfig::from_toml_file(config_path)
            .map_err(|e| ConfigErrorPy::new_err(format!("Config error: {}", e)))?;
        let db = RustDB::with_unified_config(config).map_err(map_error)?;
        Ok(Self { inner: Arc::new(db) })
    }

    // ---------- Node CRUD ----------

    /// Create a node and return its id.
    #[pyo3(signature = (label, properties=None))]
    fn create_node(
        &self,
        py: Python<'_>,
        label: &str,
        properties: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<u64> {
        let props = py_dict_to_property_map(properties)?;
        let label_owned = label.to_owned();
        let db = self.inner();
        let id = py
            .allow_threads(move || db.create_node(&label_owned, props))
            .map_err(map_error)?;
        Ok(id.as_u64())
    }

    /// Get a node by id.
    fn get_node(&self, py: Python<'_>, id: u64) -> PyResult<PyNode> {
        let nid = node_id(id)?;
        let db = self.inner();
        let node = py
            .allow_threads(move || db.get_node(nid))
            .map_err(map_error)?;
        Ok(PyNode::from_rust(node))
    }

    /// Update properties on a node (full replacement).
    #[pyo3(signature = (id, properties=None))]
    fn update_node(
        &self,
        py: Python<'_>,
        id: u64,
        properties: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let nid = node_id(id)?;
        let props = py_dict_to_property_map(properties)?;
        let db = self.inner();
        py.allow_threads(move || -> Result<(), Error> {
            db.write(|tx| tx.update_node(nid, props))
        })
        .map_err(map_error)
    }

    /// Delete a node (orphans its edges; prefer `delete_node_cascade`).
    fn delete_node(&self, py: Python<'_>, id: u64) -> PyResult<()> {
        let nid = node_id(id)?;
        let db = self.inner();
        py.allow_threads(move || -> Result<(), Error> {
            db.write(|tx| tx.delete_node(nid))
        })
        .map_err(map_error)
    }

    /// Delete a node along with all of its incident edges.
    fn delete_node_cascade(&self, py: Python<'_>, id: u64) -> PyResult<()> {
        let nid = node_id(id)?;
        let db = self.inner();
        py.allow_threads(move || -> Result<(), Error> {
            db.write(|tx| tx.delete_node_cascade(nid))
        })
        .map_err(map_error)
    }

    /// Count nodes in the current state.
    fn count_nodes(&self) -> usize {
        self.inner.node_count()
    }

    /// List all node ids in the current state.
    fn list_nodes(&self) -> Vec<u64> {
        self.inner
            .get_all_node_ids()
            .into_iter()
            .map(|id| id.as_u64())
            .collect()
    }

    /// Scan node ids by label. If `limit` is provided, return at most that many ids.
    #[pyo3(signature = (label, limit=None))]
    fn nodes_by_label(&self, label: &str, limit: Option<usize>) -> Vec<u64> {
        let iter = self.inner.scan_nodes_by_label(label).map(|id| id.as_u64());
        match limit {
            Some(n) => iter.take(n).collect(),
            None => iter.collect(),
        }
    }

    // ---------- Edge CRUD ----------

    /// Create an edge from `source` to `target` with `label` and return its id.
    #[pyo3(signature = (source, target, label, properties=None))]
    fn create_edge(
        &self,
        py: Python<'_>,
        source: u64,
        target: u64,
        label: &str,
        properties: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<u64> {
        let src = node_id(source)?;
        let tgt = node_id(target)?;
        let props = py_dict_to_property_map(properties)?;
        let label_owned = label.to_owned();
        let db = self.inner();
        let id = py
            .allow_threads(move || db.create_edge(src, tgt, &label_owned, props))
            .map_err(map_error)?;
        Ok(id.as_u64())
    }

    /// Get an edge by id.
    fn get_edge(&self, py: Python<'_>, id: u64) -> PyResult<PyEdge> {
        let eid = edge_id(id)?;
        let db = self.inner();
        let edge = py
            .allow_threads(move || db.get_edge(eid))
            .map_err(map_error)?;
        Ok(PyEdge::from_rust(edge))
    }

    #[pyo3(signature = (id, properties=None))]
    fn update_edge(
        &self,
        py: Python<'_>,
        id: u64,
        properties: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let eid = edge_id(id)?;
        let props = py_dict_to_property_map(properties)?;
        let db = self.inner();
        py.allow_threads(move || -> Result<(), Error> {
            db.write(|tx| tx.update_edge(eid, props))
        })
        .map_err(map_error)
    }

    fn delete_edge(&self, py: Python<'_>, id: u64) -> PyResult<()> {
        let eid = edge_id(id)?;
        let db = self.inner();
        py.allow_threads(move || -> Result<(), Error> {
            db.write(|tx| tx.delete_edge(eid))
        })
        .map_err(map_error)
    }

    fn count_edges(&self) -> usize {
        self.inner.edge_count()
    }

    // ---------- Adjacency / traversal ----------

    #[pyo3(signature = (node_id, label=None))]
    fn outgoing_edges(&self, node_id: u64, label: Option<&str>) -> PyResult<Vec<u64>> {
        let nid = self::node_id(node_id)?;
        let ids = match label {
            Some(l) => self.inner.get_outgoing_edges_with_label(nid, l),
            None => self.inner.get_outgoing_edges(nid),
        };
        Ok(ids.into_iter().map(|e| e.as_u64()).collect())
    }

    #[pyo3(signature = (node_id, label=None))]
    fn incoming_edges(&self, node_id: u64, label: Option<&str>) -> PyResult<Vec<u64>> {
        let nid = self::node_id(node_id)?;
        let ids = match label {
            Some(l) => self.inner.get_incoming_edges_with_label(nid, l),
            None => self.inner.get_incoming_edges(nid),
        };
        Ok(ids.into_iter().map(|e| e.as_u64()).collect())
    }

    fn out_degree(&self, node_id: u64) -> PyResult<usize> {
        let nid = self::node_id(node_id)?;
        Ok(self.inner.out_degree(nid))
    }

    fn in_degree(&self, node_id: u64) -> PyResult<usize> {
        let nid = self::node_id(node_id)?;
        Ok(self.inner.in_degree(nid))
    }

    /// BFS traversal from `start`, optionally filtered by edge label, up to `max_depth`.
    ///
    /// `direction` is one of `"out"`, `"in"`, or `"both"` (default `"out"`).
    /// Returns a list of dicts `{node_id, edge_id, depth}`.
    #[pyo3(signature = (start, label=None, max_depth=3, direction="out"))]
    fn traverse(
        &self,
        py: Python<'_>,
        start: u64,
        label: Option<&str>,
        max_depth: u32,
        direction: &str,
    ) -> PyResult<Py<PyList>> {
        use std::collections::{HashSet, VecDeque};
        let start_id = node_id(start)?;
        let direction = direction.to_lowercase();
        if !matches!(direction.as_str(), "out" | "in" | "both") {
            return Err(PyValueError::new_err(
                "direction must be 'out', 'in', or 'both'",
            ));
        }
        let db = self.inner();
        let label_opt = label.map(|s| s.to_string());

        let results: Vec<(u64, Option<u64>, u32)> = py.allow_threads(move || {
            let mut visited: HashSet<NodeId> = HashSet::new();
            let mut queue: VecDeque<(NodeId, Option<EdgeId>, u32)> = VecDeque::new();
            let mut out: Vec<(u64, Option<u64>, u32)> = Vec::new();
            queue.push_back((start_id, None, 0));
            visited.insert(start_id);
            while let Some((node, via_edge, depth)) = queue.pop_front() {
                out.push((node.as_u64(), via_edge.map(|e| e.as_u64()), depth));
                if depth >= max_depth {
                    continue;
                }
                let mut next_edges: Vec<EdgeId> = Vec::new();
                if direction == "out" || direction == "both" {
                    let edges = match &label_opt {
                        Some(l) => db.get_outgoing_edges_with_label(node, l),
                        None => db.get_outgoing_edges(node),
                    };
                    next_edges.extend(edges);
                }
                if direction == "in" || direction == "both" {
                    let edges = match &label_opt {
                        Some(l) => db.get_incoming_edges_with_label(node, l),
                        None => db.get_incoming_edges(node),
                    };
                    next_edges.extend(edges);
                }
                for eid in next_edges {
                    let next_node = if direction == "in" {
                        db.get_edge_source(eid)
                    } else if direction == "out" {
                        db.get_edge_target(eid)
                    } else {
                        // both: pick whichever endpoint isn't current node
                        match (db.get_edge_source(eid), db.get_edge_target(eid)) {
                            (Ok(s), Ok(t)) => Ok(if s == node { t } else { s }),
                            (Err(e), _) | (_, Err(e)) => Err(e),
                        }
                    };
                    if let Ok(next_node) = next_node {
                        if visited.insert(next_node) {
                            queue.push_back((next_node, Some(eid), depth + 1));
                        }
                    }
                }
            }
            out
        });

        let list = PyList::empty_bound(py);
        for (nid, eid, depth) in results {
            let d = PyDict::new_bound(py);
            d.set_item("node_id", nid)?;
            d.set_item("edge_id", eid)?;
            d.set_item("depth", depth)?;
            list.append(d)?;
        }
        Ok(list.unbind())
    }

    // ---------- Bi-temporal ----------

    /// Get a node as it existed at `valid_time` / `tx_time`.
    /// Times may be a datetime, ISO-8601 string, int micros, or `None` (now).
    #[pyo3(signature = (id, valid_time=None, tx_time=None))]
    fn get_node_at_time(
        &self,
        py: Python<'_>,
        id: u64,
        valid_time: Option<Bound<'_, PyAny>>,
        tx_time: Option<Bound<'_, PyAny>>,
    ) -> PyResult<PyNode> {
        let nid = node_id(id)?;
        let vt = match valid_time {
            Some(v) => parse_timestamp(py, &v)?,
            None => aletheiadb::time::now(),
        };
        let tt = match tx_time {
            Some(v) => parse_timestamp(py, &v)?,
            None => aletheiadb::time::now(),
        };
        let db = self.inner();
        let node = py
            .allow_threads(move || db.get_node_at_time(nid, vt, tt))
            .map_err(map_error)?;
        Ok(PyNode::from_rust(node))
    }

    /// Get an edge as it existed at `valid_time` / `tx_time`.
    #[pyo3(signature = (id, valid_time=None, tx_time=None))]
    fn get_edge_at_time(
        &self,
        py: Python<'_>,
        id: u64,
        valid_time: Option<Bound<'_, PyAny>>,
        tx_time: Option<Bound<'_, PyAny>>,
    ) -> PyResult<PyEdge> {
        let eid = edge_id(id)?;
        let vt = match valid_time {
            Some(v) => parse_timestamp(py, &v)?,
            None => aletheiadb::time::now(),
        };
        let tt = match tx_time {
            Some(v) => parse_timestamp(py, &v)?,
            None => aletheiadb::time::now(),
        };
        let db = self.inner();
        let edge = py
            .allow_threads(move || db.get_edge_at_time(eid, vt, tt))
            .map_err(map_error)?;
        Ok(PyEdge::from_rust(edge))
    }

    /// Return the full history of a node as a list of `{version_id, label, properties, valid_from, valid_to, tx_from, tx_to}`.
    fn node_history(&self, py: Python<'_>, id: u64) -> PyResult<Py<PyList>> {
        let nid = node_id(id)?;
        let db = self.inner();
        let history = py
            .allow_threads(move || db.get_node_history(nid))
            .map_err(map_error)?;
        let list = PyList::empty_bound(py);
        for v in history.versions.iter() {
            let d = PyDict::new_bound(py);
            d.set_item("version_id", v.version_id.as_u64())?;
            d.set_item("version_number", v.version_number)?;
            d.set_item("label", v.label.clone())?;
            d.set_item("properties", property_map_to_py_dict(py, &v.properties)?)?;
            let vt = v.temporal.valid_time();
            let tt = v.temporal.transaction_time();
            d.set_item("valid_from_micros", vt.start().wallclock())?;
            d.set_item("valid_to_micros", vt.end().wallclock())?;
            d.set_item("tx_from_micros", tt.start().wallclock())?;
            d.set_item("tx_to_micros", tt.end().wallclock())?;
            list.append(d)?;
        }
        Ok(list.unbind())
    }

    // ---------- AQL / Cypher ----------

    /// Execute a Cypher query and return rows as a list of dicts.
    #[pyo3(signature = (query, params=None))]
    fn execute_cypher(
        &self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyList>> {
        crate::query::execute_cypher(py, &self.inner, query, params)
    }

    fn __repr__(&self) -> String {
        format!(
            "AletheiaDB(nodes={}, edges={})",
            self.inner.node_count(),
            self.inner.edge_count()
        )
    }
}
