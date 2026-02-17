//! Transaction types and metadata

use crate::core::temporal::Timestamp;
// Re-export TxId and TxIdGenerator from core to break dependency cycles
pub use crate::core::id::{TxId, TxIdGenerator};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tx_id_creation() {
        let tx_id = TxId::new(42).unwrap();
        assert_eq!(tx_id.as_u64(), 42u64);
    }

    #[test]
    fn test_tx_id_ordering() {
        let tx1 = TxId::new(1).unwrap();
        let tx2 = TxId::new(2).unwrap();
        assert!(tx1 < tx2);
        assert_eq!(tx1, tx1);
    }

    #[test]
    fn test_tx_id_display() {
        let tx_id = TxId::new(123).unwrap();
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
        let tx1 = generator.next().unwrap();
        let tx2 = generator.next().unwrap();
        let tx3 = generator.next().unwrap();

        assert_eq!(tx1.as_u64(), 1u64);
        assert_eq!(tx2.as_u64(), 2u64);
        assert_eq!(tx3.as_u64(), 3u64);
        assert_eq!(generator.current().as_u64(), 3u64);
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
                    ids.push(generator_clone.next().unwrap());
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
        assert_eq!(unique_count, 1000);

        // Final current should be 1000
        assert_eq!(generator.current().as_u64(), 1000u64);
    }

    #[test]
    fn test_tx_metadata() {
        let metadata = TxMetadata {
            tx_id: TxId::new(1).unwrap(),
            start_timestamp: 100.into(),
            commit_timestamp: None,
            state: TxState::Active,
            is_read_only: false,
        };

        assert_eq!(metadata.tx_id, TxId::new(1).unwrap());
        assert_eq!(metadata.start_timestamp, 100.into());
        assert_eq!(metadata.commit_timestamp, None);
        assert_eq!(metadata.state, TxState::Active);
        assert!(!metadata.is_read_only);
    }
}
