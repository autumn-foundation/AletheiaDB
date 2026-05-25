pub mod core_error {
    pub struct StorageError;
}
pub mod storage_error {
    pub struct IndexPersistenceError;
    impl From<IndexPersistenceError> for super::core_error::StorageError {
        fn from(_: IndexPersistenceError) -> Self {
            super::core_error::StorageError
        }
    }
}
