//! Embedding provider implementations.
//!
//! This module contains concrete implementations of the `EmbeddingProvider` trait
//! for various embedding services and local models.

#[cfg(feature = "embedding-openai")]
pub mod openai;

#[cfg(feature = "embedding-huggingface")]
pub mod huggingface;

#[cfg(feature = "embedding-onnx")]
pub mod onnx;

#[cfg(feature = "embedding-ollama")]
pub mod ollama;
