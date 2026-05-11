//! Python bindings for AletheiaDB (extension module `aletheiadb._native`).

use pyo3::prelude::*;

mod db;
mod errors;
mod query;
mod types;
mod vector;

#[pymodule]
fn _native(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;

    module.add_class::<db::PyAletheiaDB>()?;
    module.add_class::<types::PyNode>()?;
    module.add_class::<types::PyEdge>()?;
    module.add_class::<vector::PyHnswConfig>()?;
    module.add_class::<vector::PyDistanceMetric>()?;

    module.add_function(wrap_pyfunction!(vector::enable_vector_index, module)?)?;
    module.add_function(wrap_pyfunction!(vector::find_similar, module)?)?;
    module.add_function(wrap_pyfunction!(vector::find_similar_by_vector, module)?)?;
    module.add_function(wrap_pyfunction!(query::execute_aql, module)?)?;

    errors::register(py, module)?;
    Ok(())
}
