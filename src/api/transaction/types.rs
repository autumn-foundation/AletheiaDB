//! Transaction types and metadata

use crate::core::temporal::Timestamp;
use std::sync::atomic::{AtomicU64, Ordering};

/// Transaction ID - globally unique identifier for transactions
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxId(u64);

impl TxId {
    /// Create a new transaction ID
    pub fn new(id: u64) -> Self {
        TxId(id)
    }

    /// Get the inner ID value
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for TxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TxId({})", self.0)
    }
}

/// Transaction state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    /// Transaction is active and can perform operations
    Active,
    /// Transaction is preparing to commit (validating)
    Preparing,
    /// Transaction has committed successfully
    Committed,
    /// Transaction has been rolled back
    Aborted,
}

impl std::fmt::Display for TxState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxState::Active => write!(f, "Active"),
            TxState::Preparing => write!(f, "Preparing"),
            TxState::Committed => write!(f, "Committed"),
            TxState::Aborted => write!(f, "Aborted"),
        }
    }
}

/// Transaction metadata
#[derive(Debug, Clone)]
pub struct TxMetadata {
    /// Transaction ID
    pub tx_id: TxId,
    /// Timestamp when transaction started
    pub start_timestamp: Timestamp,
    /// Timestamp when transaction committed (None if not yet committed)
    pub commit_timestamp: Option<Timestamp>,
    /// Current transaction state
    pub state: TxState,
    /// Whether this is a read-only transaction
    pub is_read_only: bool,
}

/// Global transaction ID generator
///
/// Generates monotonically increasing transaction IDs using atomic operations.
pub struct TxIdGenerator {
    counter: AtomicU64,
}

impl TxIdGenerator {
    /// Create a new transaction ID generator starting from 1
    pub fn new() -> Self {
        TxIdGenerator {
            counter: AtomicU64::new(1),
        }
    }

    /// Generate the next transaction ID
    ///
    /// This operation is atomic and thread-safe.
    pub fn next(&self) -> TxId {
        TxId(self.counter.fetch_add(1, Ordering::SeqCst))
    }

    /// Get the current transaction ID (last generated)
    pub fn current(&self) -> TxId {
        TxId(self.counter.load(Ordering::SeqCst).saturating_sub(1))
    }
}

impl Default for TxIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tx_id_creation() {
        let tx_id = TxId::new(42);
        assert_eq!(tx_id.as_u64(), 42.into());
    }

    #[test]
    fn test_tx_id_ordering() {
        let tx1 = TxId::new(1);
        let tx2 = TxId::new(2);
        assert!(tx1 < tx2);
        assert_eq!(tx1, tx1);
    }

    #[test]
    fn test_tx_id_display() {
        let tx_id = TxId::new(123);
        assert_eq!(format!("{}", tx_id), "TxId(123)");
    }

    #[test]
    fn test_tx_state_display() {
        assert_eq!(format!("{}", TxState::Active), "Active");
        assert_eq!(format!("{}", TxState::Preparing), "Preparing");
        assert_eq!(format!("{}", TxState::Committed), "Committed");
        assert_eq!(format!("{}", TxState::Aborted), "Aborted");
    }

    #[test]
    fn test_tx_id_generator() {
        let generator = TxIdGenerator::new();
        let tx1 = generator.next();
        let tx2 = generator.next();
        let tx3 = generator.next();

        assert_eq!(tx1.as_u64(), 1.into());
        assert_eq!(tx2.as_u64(), 2.into());
        assert_eq!(tx3.as_u64(), 3.into());
        assert_eq!(generator.current().as_u64(), 3.into());
    }

    #[test]
    fn test_tx_id_generator_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let generator = Arc::new(TxIdGenerator::new());
        let mut handles = vec![];

        // Spawn 10 threads, each generating 100 IDs
        for _ in 0..10 {
            let generator_clone = Arc::clone(&generator);
            let handle = thread::spawn(move || {
                let mut ids = vec![];
                for _ in 0..100 {
                    ids.push(generator_clone.next());
                }
                ids
            });
            handles.push(handle);
        }

        // Collect all generated IDs
        let mut all_ids: Vec<TxId> = vec![];
        for handle in handles {
            all_ids.extend(handle.join().unwrap());
        }

        // All IDs should be unique
        all_ids.sort();
        let unique_count = all_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert_eq!(unique_count, 1000.into());

        // Final current should be 1000
        assert_eq!(generator.current().as_u64(), 1000.into());
    }

    #[test]
    fn test_tx_metadata() {
        let metadata = TxMetadata {
            tx_id: TxId::new(1),
            start_timestamp: 100.into(),
            commit_timestamp: None,
            state: TxState::Active,
            is_read_only: false,
        };

        assert_eq!(metadata.tx_id, TxId::new(1));
        assert_eq!(metadata.start_timestamp, 100.into());
        assert_eq!(metadata.commit_timestamp, None);
        assert_eq!(metadata.state, TxState::Active);
        assert!(!metadata.is_read_only);
    }
}
