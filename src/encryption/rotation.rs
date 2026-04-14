//! Key rotation mechanism for encryption at rest.
//!
//! Supports atomic key version switching without downtime.
//! Background re-encryption of existing data is planned for a future phase.

use crate::encryption::KeyProviderError;
use std::sync::RwLock;

/// Tracks key version information.
#[derive(Debug, Clone)]
pub struct KeyVersion {
    /// Monotonically increasing version number.
    pub version: u32,
    /// UUID for this key version (for file headers).
    pub version_id: [u8; 16],
}

/// State of a key rotation operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationState {
    /// No rotation in progress.
    Idle,
    /// Rotation initiated, new key active for writes.
    InProgress {
        /// Old key version.
        old_version: u32,
        /// New key version.
        new_version: u32,
    },
    /// Rotation complete, old key removed.
    Complete,
}

/// Manages key rotation for the encryption system.
///
/// Holds the current key version and supports atomic key switching.
/// Background re-encryption of existing data is planned for a future phase.
pub struct KeyRotationManager {
    /// Current key version counter.
    current_version: RwLock<u32>,
    /// Current rotation state.
    state: RwLock<RotationState>,
}

impl KeyRotationManager {
    /// Create a new rotation manager starting at version 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            current_version: RwLock::new(1),
            state: RwLock::new(RotationState::Idle),
        }
    }

    /// Get the current key version.
    #[must_use]
    pub fn current_version(&self) -> u32 {
        *self
            .current_version
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Get the current rotation state.
    #[must_use]
    pub fn state(&self) -> RotationState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Begin a key rotation.
    ///
    /// Returns the new version number on success.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError::Unavailable`] if a rotation is already in progress.
    pub fn begin_rotation(&self) -> Result<u32, KeyProviderError> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if *state != RotationState::Idle {
            return Err(KeyProviderError::Unavailable(
                "Key rotation already in progress".into(),
            ));
        }
        let old = self.current_version();
        let new = old + 1;
        *state = RotationState::InProgress {
            old_version: old,
            new_version: new,
        };
        Ok(new)
    }

    /// Complete the current rotation, advancing the version counter.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError::Unavailable`] if no rotation is in progress.
    pub fn complete_rotation(&self) -> Result<(), KeyProviderError> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        match *state {
            RotationState::InProgress { new_version, .. } => {
                let mut ver = self
                    .current_version
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                *ver = new_version;
                *state = RotationState::Idle;
                Ok(())
            }
            _ => Err(KeyProviderError::Unavailable(
                "No rotation in progress".into(),
            )),
        }
    }

    /// Cancel the current rotation (rollback).
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError::Unavailable`] if no rotation is in progress.
    pub fn cancel_rotation(&self) -> Result<(), KeyProviderError> {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        match *state {
            RotationState::InProgress { .. } => {
                *state = RotationState::Idle;
                Ok(())
            }
            _ => Err(KeyProviderError::Unavailable(
                "No rotation in progress".into(),
            )),
        }
    }
}

impl Default for KeyRotationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_at_version_1() {
        let mgr = KeyRotationManager::new();
        assert_eq!(mgr.current_version(), 1);
        assert_eq!(mgr.state(), RotationState::Idle);
    }

    #[test]
    fn begin_rotation_transitions_state() {
        let mgr = KeyRotationManager::new();
        let new_ver = mgr.begin_rotation().unwrap();
        assert_eq!(new_ver, 2);
        assert_eq!(
            mgr.state(),
            RotationState::InProgress {
                old_version: 1,
                new_version: 2,
            }
        );
        // Version counter is not advanced until complete.
        assert_eq!(mgr.current_version(), 1);
    }

    #[test]
    fn complete_rotation_advances_version() {
        let mgr = KeyRotationManager::new();
        mgr.begin_rotation().unwrap();
        mgr.complete_rotation().unwrap();

        assert_eq!(mgr.current_version(), 2);
        assert_eq!(mgr.state(), RotationState::Idle);
    }

    #[test]
    fn cancel_rotation_returns_to_idle() {
        let mgr = KeyRotationManager::new();
        mgr.begin_rotation().unwrap();
        mgr.cancel_rotation().unwrap();

        assert_eq!(mgr.current_version(), 1);
        assert_eq!(mgr.state(), RotationState::Idle);
    }

    #[test]
    fn double_begin_fails() {
        let mgr = KeyRotationManager::new();
        mgr.begin_rotation().unwrap();
        let err = mgr.begin_rotation().unwrap_err();
        assert!(
            err.to_string().contains("already in progress"),
            "expected 'already in progress', got: {err}"
        );
    }

    #[test]
    fn complete_without_begin_fails() {
        let mgr = KeyRotationManager::new();
        let err = mgr.complete_rotation().unwrap_err();
        assert!(
            err.to_string().contains("No rotation in progress"),
            "expected 'No rotation in progress', got: {err}"
        );
    }

    #[test]
    fn cancel_without_begin_fails() {
        let mgr = KeyRotationManager::new();
        let err = mgr.cancel_rotation().unwrap_err();
        assert!(
            err.to_string().contains("No rotation in progress"),
            "expected 'No rotation in progress', got: {err}"
        );
    }

    #[test]
    fn multiple_rotation_cycles() {
        let mgr = KeyRotationManager::new();

        // Cycle 1
        mgr.begin_rotation().unwrap();
        mgr.complete_rotation().unwrap();
        assert_eq!(mgr.current_version(), 2);

        // Cycle 2
        mgr.begin_rotation().unwrap();
        mgr.complete_rotation().unwrap();
        assert_eq!(mgr.current_version(), 3);

        // Cycle 3 with cancel
        mgr.begin_rotation().unwrap();
        mgr.cancel_rotation().unwrap();
        assert_eq!(mgr.current_version(), 3);

        // Cycle 4 after cancel
        mgr.begin_rotation().unwrap();
        mgr.complete_rotation().unwrap();
        assert_eq!(mgr.current_version(), 4);
    }

    #[test]
    fn key_version_struct() {
        let kv = KeyVersion {
            version: 42,
            version_id: [0xAA; 16],
        };
        assert_eq!(kv.version, 42);
        assert_eq!(kv.version_id, [0xAA; 16]);

        // Clone works.
        let kv2 = kv.clone();
        assert_eq!(kv2.version, kv.version);
    }

    #[test]
    fn default_creates_same_as_new() {
        let mgr = KeyRotationManager::default();
        assert_eq!(mgr.current_version(), 1);
        assert_eq!(mgr.state(), RotationState::Idle);
    }
}
