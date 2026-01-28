//! Security primitives for GallifreyDB.
//!
//! This module handles encryption key management via the [`KeyProvider`] trait.

/// Key provider trait and basic implementations.
pub mod key_provider;
pub use key_provider::*;

/// File-based key provider.
pub mod file_provider;
pub use file_provider::*;

/// Environment variable key provider.
pub mod env_provider;
pub use env_provider::*;

/// AWS KMS key provider.
#[cfg(feature = "aws-kms")]
pub mod kms;
#[cfg(feature = "aws-kms")]
pub use kms::*;

/// HashiCorp Vault key provider.
#[cfg(feature = "vault")]
pub mod vault;
#[cfg(feature = "vault")]
pub use vault::*;

#[cfg(test)]
mod tests;
