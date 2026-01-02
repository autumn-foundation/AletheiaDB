//! Lock utility extensions for graceful error handling.
//!
//! This module provides extension traits for `Mutex` and `RwLock` that return
//! `Result` types instead of panicking on lock poisoning. This prevents cascade
//! failures where a single panicking thread causes all subsequent lock acquisitions
//! to panic.
//!
//! # Lock Poisoning
//!
//! In Rust, when a thread panics while holding a lock, the lock becomes "poisoned".
//! By default, attempting to acquire a poisoned lock will panic. This module provides
//! methods that return errors instead, allowing callers to handle the situation
//! gracefully.
//!
//! # Usage
//!
//! ```ignore
//! use gallifreydb::utils::lock::LockExt;
//!
//! let mutex = Mutex::new(42);
//! let guard = mutex.lock_or_err()?;
//! ```

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::error::{Error, StorageError};

/// Extension trait for `Mutex` providing graceful error handling.
pub trait MutexExt<T> {
    /// Acquire the lock, returning an error if the lock is poisoned.
    ///
    /// This method is equivalent to `lock().unwrap()` but returns a `Result`
    /// instead of panicking, allowing callers to handle lock poisoning gracefully.
    ///
    /// # Errors
    ///
    /// Returns `Error::Storage(StorageError::LockPoisoned)` if the lock has been
    /// poisoned by a panicking thread.
    fn lock_or_err(&self) -> Result<MutexGuard<'_, T>, Error>;

    /// Acquire the lock, recovering the data even if poisoned.
    ///
    /// This method acquires the lock regardless of whether it's poisoned,
    /// using `into_inner()` to extract the data. Use with caution - the
    /// protected data may be in an inconsistent state if a thread panicked
    /// while modifying it.
    ///
    /// # Safety Considerations
    ///
    /// Only use this method when:
    /// - The protected data has no invariants that could be violated
    /// - You can validate/repair the data after acquisition
    /// - Availability is more important than consistency
    fn lock_or_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_or_err(&self) -> Result<MutexGuard<'_, T>, Error> {
        self.lock().map_err(|_| {
            StorageError::LockPoisoned {
                lock_type: "Mutex".to_string(),
            }
            .into()
        })
    }

    fn lock_or_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Extension trait for `RwLock` providing graceful error handling.
pub trait RwLockExt<T> {
    /// Acquire a read lock, returning an error if the lock is poisoned.
    ///
    /// This method is equivalent to `read().unwrap()` but returns a `Result`
    /// instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns `Error::Storage(StorageError::LockPoisoned)` if the lock has been
    /// poisoned by a panicking thread.
    fn read_or_err(&self) -> Result<RwLockReadGuard<'_, T>, Error>;

    /// Acquire a write lock, returning an error if the lock is poisoned.
    ///
    /// This method is equivalent to `write().unwrap()` but returns a `Result`
    /// instead of panicking.
    ///
    /// # Errors
    ///
    /// Returns `Error::Storage(StorageError::LockPoisoned)` if the lock has been
    /// poisoned by a panicking thread.
    fn write_or_err(&self) -> Result<RwLockWriteGuard<'_, T>, Error>;

    /// Acquire a read lock, recovering even if poisoned.
    ///
    /// Use with caution - see `MutexExt::lock_or_recover` for safety considerations.
    fn read_or_recover(&self) -> RwLockReadGuard<'_, T>;

    /// Acquire a write lock, recovering even if poisoned.
    ///
    /// Use with caution - see `MutexExt::lock_or_recover` for safety considerations.
    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_or_err(&self) -> Result<RwLockReadGuard<'_, T>, Error> {
        self.read().map_err(|_| {
            StorageError::LockPoisoned {
                lock_type: "RwLock".to_string(),
            }
            .into()
        })
    }

    fn write_or_err(&self) -> Result<RwLockWriteGuard<'_, T>, Error> {
        self.write().map_err(|_| {
            StorageError::LockPoisoned {
                lock_type: "RwLock".to_string(),
            }
            .into()
        })
    }

    fn read_or_recover(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_or_recover(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_mutex_lock_or_err_success() {
        let mutex = Mutex::new(42);
        let guard = mutex.lock_or_err().unwrap();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_rwlock_read_or_err_success() {
        let rwlock = RwLock::new(42);
        let guard = rwlock.read_or_err().unwrap();
        assert_eq!(*guard, 42);
    }

    #[test]
    fn test_rwlock_write_or_err_success() {
        let rwlock = RwLock::new(42);
        let mut guard = rwlock.write_or_err().unwrap();
        *guard = 100;
        drop(guard);
        assert_eq!(*rwlock.read_or_err().unwrap(), 100);
    }

    #[test]
    fn test_mutex_lock_or_err_poisoned() {
        let mutex = Arc::new(Mutex::new(42));
        let mutex_clone = Arc::clone(&mutex);

        // Panic while holding the lock to poison it
        let handle = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("intentional panic to poison lock");
        });

        // Wait for the thread to finish (it will panic)
        let _ = handle.join();

        // Now try to acquire the poisoned lock
        let result = mutex.lock_or_err();
        assert!(result.is_err());

        // Verify it's the right error type
        if let Err(Error::Storage(StorageError::LockPoisoned { lock_type })) = result {
            assert_eq!(lock_type, "Mutex");
        } else {
            panic!("Expected LockPoisoned error");
        }
    }

    #[test]
    fn test_mutex_lock_or_recover_poisoned() {
        let mutex = Arc::new(Mutex::new(42));
        let mutex_clone = Arc::clone(&mutex);

        // Panic while holding the lock to poison it
        let handle = thread::spawn(move || {
            let mut guard = mutex_clone.lock().unwrap();
            *guard = 100; // Modify before panic
            panic!("intentional panic to poison lock");
        });

        // Wait for the thread to finish (it will panic)
        let _ = handle.join();

        // Should be able to recover the data
        let guard = mutex.lock_or_recover();
        assert_eq!(*guard, 100);
    }

    #[test]
    fn test_rwlock_read_or_err_poisoned() {
        let rwlock = Arc::new(RwLock::new(42));
        let rwlock_clone = Arc::clone(&rwlock);

        // Panic while holding a write lock to poison it
        let handle = thread::spawn(move || {
            let _guard = rwlock_clone.write().unwrap();
            panic!("intentional panic to poison lock");
        });

        let _ = handle.join();

        let result = rwlock.read_or_err();
        assert!(result.is_err());
    }

    #[test]
    fn test_rwlock_write_or_err_poisoned() {
        let rwlock = Arc::new(RwLock::new(42));
        let rwlock_clone = Arc::clone(&rwlock);

        let handle = thread::spawn(move || {
            let _guard = rwlock_clone.write().unwrap();
            panic!("intentional panic to poison lock");
        });

        let _ = handle.join();

        let result = rwlock.write_or_err();
        assert!(result.is_err());
    }

    #[test]
    fn test_rwlock_read_or_recover_poisoned() {
        let rwlock = Arc::new(RwLock::new(42));
        let rwlock_clone = Arc::clone(&rwlock);

        let handle = thread::spawn(move || {
            let mut guard = rwlock_clone.write().unwrap();
            *guard = 100;
            panic!("intentional panic to poison lock");
        });

        let _ = handle.join();

        let guard = rwlock.read_or_recover();
        assert_eq!(*guard, 100);
    }

    #[test]
    fn test_rwlock_write_or_recover_poisoned() {
        let rwlock = Arc::new(RwLock::new(42));
        let rwlock_clone = Arc::clone(&rwlock);

        let handle = thread::spawn(move || {
            let mut guard = rwlock_clone.write().unwrap();
            *guard = 100;
            panic!("intentional panic to poison lock");
        });

        let _ = handle.join();

        let mut guard = rwlock.write_or_recover();
        assert_eq!(*guard, 100);
        *guard = 200;
        drop(guard);
        assert_eq!(*rwlock.read_or_recover(), 200);
    }
}
