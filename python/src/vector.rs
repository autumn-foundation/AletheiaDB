//! Vector indexing bindings.

use crate::db::PyAletheiaDB;
use crate::errors::map_error;
use aletheiadb::core::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyList;

/// Distance metrics for vector similarity. Use the `COSINE`, `EUCLIDEAN`, or `DOT_PRODUCT` constants.
#[pyclass(name = "DistanceMetric", module = "aletheiadb._native", frozen, eq)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PyDistanceMetric {
    pub(crate) inner: DistanceMetric,
}

#[pymethods]
impl PyDistanceMetric {
    #[classattr]
    const COSINE: PyDistanceMetric = PyDistanceMetric {
        inner: DistanceMetric::Cosine,
    };
    #[classattr]
    const EUCLIDEAN: PyDistanceMetric = PyDistanceMetric {
        inner: DistanceMetric::Euclidean,
    };
    #[classattr]
    const DOT_PRODUCT: PyDistanceMetric = PyDistanceMetric {
        inner: DistanceMetric::DotProduct,
    };
    #[classattr]
    const HAVERSINE: PyDistanceMetric = PyDistanceMetric {
        inner: DistanceMetric::Haversine,
    };
    #[classattr]
    const HAMMING: PyDistanceMetric = PyDistanceMetric {
        inner: DistanceMetric::Hamming,
    };
    #[classattr]
    const TANIMOTO: PyDistanceMetric = PyDistanceMetric {
        inner: DistanceMetric::Tanimoto,
    };

    fn __repr__(&self) -> &'static str {
        match self.inner {
            DistanceMetric::Cosine => "DistanceMetric.COSINE",
            DistanceMetric::Euclidean => "DistanceMetric.EUCLIDEAN",
            DistanceMetric::DotProduct => "DistanceMetric.DOT_PRODUCT",
            DistanceMetric::Haversine => "DistanceMetric.HAVERSINE",
            DistanceMetric::Hamming => "DistanceMetric.HAMMING",
            DistanceMetric::Tanimoto => "DistanceMetric.TANIMOTO",
        }
    }
}

/// HNSW index configuration.
#[pyclass(name = "HnswConfig", module = "aletheiadb._native")]
#[derive(Clone)]
pub struct PyHnswConfig {
    pub(crate) inner: HnswConfig,
}

#[pymethods]
impl PyHnswConfig {
    #[new]
    #[pyo3(signature = (dimensions, metric=None, m=None, ef_construction=None, ef_search=None))]
    fn new(
        dimensions: usize,
        metric: Option<PyDistanceMetric>,
        m: Option<usize>,
        ef_construction: Option<usize>,
        ef_search: Option<usize>,
    ) -> Self {
        let metric = metric.map(|m| m.inner).unwrap_or(DistanceMetric::Cosine);
        let mut cfg = HnswConfig::new(dimensions, metric);
        if let Some(v) = m {
            cfg = cfg.with_m(v);
        }
        if let Some(v) = ef_construction {
            cfg = cfg.with_ef_construction(v);
        }
        if let Some(v) = ef_search {
            cfg = cfg.with_ef_search(v);
        }
        Self { inner: cfg }
    }

    #[getter]
    fn dimensions(&self) -> usize {
        self.inner.dimensions
    }

    #[getter]
    fn metric(&self) -> PyDistanceMetric {
        PyDistanceMetric {
            inner: self.inner.metric,
        }
    }

    fn __repr__(&self) -> String {
        let metric = match self.inner.metric {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::Euclidean => "euclidean",
            DistanceMetric::DotProduct => "dot_product",
            DistanceMetric::Haversine => "haversine",
            DistanceMetric::Hamming => "hamming",
            DistanceMetric::Tanimoto => "tanimoto",
        };
        format!(
            "HnswConfig(dimensions={}, metric={:?}, m={}, ef_construction={}, ef_search={})",
            self.inner.dimensions,
            metric,
            self.inner.m,
            self.inner.ef_construction,
            self.inner.ef_search,
        )
    }
}

/// Enable an HNSW vector index on `property` of the given database.
#[pyfunction]
pub fn enable_vector_index(
    py: Python<'_>,
    db: &PyAletheiaDB,
    property: &str,
    config: &PyHnswConfig,
) -> PyResult<()> {
    let rust_db = db.inner();
    let property = property.to_string();
    let config = config.inner.clone();
    py.allow_threads(move || rust_db.vector_index(&property).hnsw(config).enable())
        .map_err(map_error)
}

/// Find the `k` nearest neighbours to `query_node_id` using the enabled vector index.
#[pyfunction]
#[pyo3(signature = (db, query_node_id, k=10, label=None))]
pub fn find_similar(
    py: Python<'_>,
    db: &PyAletheiaDB,
    query_node_id: u64,
    k: usize,
    label: Option<&str>,
) -> PyResult<Py<PyList>> {
    let nid = NodeId::new(query_node_id).map_err(|e| crate::errors::map_storage_error(e))?;
    let rust_db = db.inner();
    let label_owned = label.map(|s| s.to_string());
    let results = py
        .allow_threads(move || {
            let mut query = aletheiadb::SimilarityQuery::from_node(nid).k(k);
            if let Some(l) = &label_owned {
                query = query.with_label(l);
            }
            rust_db.similarity_search(query)
        })
        .map_err(map_error)?;
    let list = PyList::empty_bound(py);
    for (id, score) in results {
        let t = (id.as_u64(), score as f64);
        list.append(t)?;
    }
    Ok(list.unbind())
}

/// Find the `k` nearest neighbours to a raw query embedding.
#[pyfunction]
#[pyo3(signature = (db, embedding, k=10, label=None))]
pub fn find_similar_by_vector(
    py: Python<'_>,
    db: &PyAletheiaDB,
    embedding: Vec<f32>,
    k: usize,
    label: Option<&str>,
) -> PyResult<Py<PyList>> {
    if embedding.is_empty() {
        return Err(PyValueError::new_err("embedding must be non-empty"));
    }
    let rust_db = db.inner();
    let label_owned = label.map(|s| s.to_string());
    let results = py
        .allow_threads(move || {
            let mut query = aletheiadb::SimilarityQuery::from_embedding(embedding).k(k);
            if let Some(l) = &label_owned {
                query = query.with_label(l);
            }
            rust_db.similarity_search(query)
        })
        .map_err(map_error)?;
    let list = PyList::empty_bound(py);
    for (id, score) in results {
        let t = (id.as_u64(), score as f64);
        list.append(t)?;
    }
    Ok(list.unbind())
}
