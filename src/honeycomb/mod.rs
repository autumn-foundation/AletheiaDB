//! Custom Honeycomb client implementation.
//!
//! This module provides a minimal, security-focused Honeycomb client that replaces
//! the `libhoney-rust` git dependency with maintained code using modern, auditable
//! dependencies.
//!
//! # Features
//!
//! - Event batching and transmission to Honeycomb API
//! - HTTP client using `reqwest 0.11+` with TLS and connection pooling
//! - Configuration compatible with `tracing-honeycomb`
//! - Async/tokio support
//! - Error handling and exponential backoff retry logic
//! - JSON serialization
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::honeycomb::{Config, Client, Event};
//!
//! let config = Config::default()
//!     .with_api_key("your-api-key")
//!     .with_dataset("my-dataset");
//!
//! let client = Client::new(config);
//!
//! let mut event = Event::new();
//! event.add_field("message", "Hello from GallifreyDB!");
//! event.add_field("duration_ms", 42);
//!
//! client.send(event).await?;
//! ```

mod batch;
mod client;
mod config;
mod error;
mod event;

pub use batch::BatchBuffer;
pub use client::Client;
pub use config::{Config, Options, TransmissionOptions};
pub use error::{Error, Result};
pub use event::Event;

/// Module containing client-related types for API compatibility with libhoney-rust.
pub mod client_types {
    pub use super::config::Options;
}

/// Module containing transmission-related types for API compatibility with libhoney-rust.
pub mod transmission {
    pub use super::config::TransmissionOptions as Options;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_exports() {
        // Verify all public types are accessible
        let _config: Config = Config::default();
        let _options: Options = Options::default();
        let _tx_options: TransmissionOptions = TransmissionOptions::default();
        let _event: Event = Event::new();
    }
}
