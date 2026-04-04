//! Embedding provider implementations.
//!
//! This module contains concrete implementations of the `EmbeddingProvider` trait
//! for various embedding services and local models.

#[cfg(feature = "embedding-openai")]
mod openai;

#[cfg(feature = "embedding-huggingface")]
mod huggingface;

#[cfg(feature = "embedding-onnx")]
mod onnx;

#[cfg(feature = "embedding-ollama")]
mod ollama;

#[cfg(feature = "embedding-openai")]
pub use openai::{OpenAIConfig, OpenAIModel, OpenAIProvider};

#[cfg(feature = "embedding-huggingface")]
pub use huggingface::{HuggingFaceConfig, HuggingFaceProvider};

#[cfg(feature = "embedding-onnx")]
pub use onnx::{OnnxConfig, OnnxModel, OnnxProvider};

#[cfg(feature = "embedding-ollama")]
pub use ollama::{OllamaConfig, OllamaProvider};
