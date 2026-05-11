//! Map AletheiaDB Rust errors to Python exceptions.

use aletheiadb::core::error::{Error, StorageError};
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(_native, AletheiaDBError, PyException);
create_exception!(_native, NodeNotFound, AletheiaDBError);
create_exception!(_native, EdgeNotFound, AletheiaDBError);
create_exception!(_native, InvalidId, AletheiaDBError);
create_exception!(_native, IoError, AletheiaDBError);
create_exception!(_native, TemporalErrorPy, AletheiaDBError);
create_exception!(_native, QueryErrorPy, AletheiaDBError);
create_exception!(_native, ConfigErrorPy, AletheiaDBError);

pub fn map_error(err: Error) -> PyErr {
    let msg = err.to_string();
    match err {
        Error::Storage(StorageError::NodeNotFound(_)) => NodeNotFound::new_err(msg),
        Error::Storage(StorageError::EdgeNotFound(_)) => EdgeNotFound::new_err(msg),
        Error::Storage(StorageError::InvalidId { .. }) => InvalidId::new_err(msg),
        Error::Storage(StorageError::IoError(_)) => IoError::new_err(msg),
        Error::Io(_) => IoError::new_err(msg),
        Error::Temporal(_) => TemporalErrorPy::new_err(msg),
        Error::Query(_) => QueryErrorPy::new_err(msg),
        _ => AletheiaDBError::new_err(msg),
    }
}

pub fn map_storage_error(err: StorageError) -> PyErr {
    map_error(Error::Storage(err))
}

pub fn register(py: Python<'_>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("AletheiaDBError", py.get_type_bound::<AletheiaDBError>())?;
    module.add("NodeNotFound", py.get_type_bound::<NodeNotFound>())?;
    module.add("EdgeNotFound", py.get_type_bound::<EdgeNotFound>())?;
    module.add("InvalidId", py.get_type_bound::<InvalidId>())?;
    module.add("IoError", py.get_type_bound::<IoError>())?;
    module.add("TemporalError", py.get_type_bound::<TemporalErrorPy>())?;
    module.add("QueryError", py.get_type_bound::<QueryErrorPy>())?;
    module.add("ConfigError", py.get_type_bound::<ConfigErrorPy>())?;
    Ok(())
}
