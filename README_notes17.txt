I can change `fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::IoError(format!("Failed to read config file '{}': {}", path.as_ref().display(), e)))?;`

Are there any other code fixes I should do?
What about the `Cargo.toml` dependencies (`embedding-onnx`, `ort`, `tokenizers`)?
I'll definitely remove them from `Cargo.toml`.

Let's summarize the PR I will create:
**Title**: 🗣️ Echo: Getting Started example is broken
**Description**:
🤦 **The Confusion:**
1. I tried to use the `embedding-onnx` feature mentioned in the README, but it seems to bring in unmaintained crates with vulnerabilities (`paste` crate). Wait, actually, the README says it's an "(⚠️ placeholder)", but it shouldn't be there at all!
2. When trying to load a nonexistent config file (`AletheiaDBConfig::from_toml_file("nonexistent.toml")`), I got an unhelpful `IoError("No such file or directory (os error 2)")` which didn't tell me which file was missing.
3. The README example for "Embedding Generation (Optional)" has `Result<(), Box<dyn std::error::Error>>` but if I had imported `aletheiadb::prelude::*`, it fails with `E0107: type alias takes 1 generic argument but 2 generic arguments were supplied`. Also, running it without adding `tokio` gives `unresolved module tokio` because there's no feature flag warning in the code itself.
4. The Vector Search example imports `TemporalVectorConfig` from a super deep path `aletheiadb::index::vector::temporal::TemporalVectorConfig`, which is annoying since it's already available at the crate root.
5. The terminology is super jargon-heavy. "Anchor+Delta Compression" should just be "snapshots and incremental backups", and "valid time/transaction time" are confusing for beginners.

🕵️ **The Reality:**
1. The `embedding-onnx` feature, along with `ort` and `tokenizers`, were non-functional placeholders that bring in the `paste` crate vulnerability (RUSTSEC-2024-0436). They should not exist in `Cargo.toml`.
2. The config reader in `src/config.rs` wraps `fs::read_to_string` error with just `e.to_string()`, losing the file path context completely.
3. & 4. & 5. These are purely docs and API usage friction points that make the DX worse.

💡 **The Fix:**
- Addressed code issues: Removed `embedding-onnx`, `ort`, and `tokenizers` from `Cargo.toml` and `src/lib.rs` to fix the vulnerability footprint.
- Improved the `AletheiaDBConfig::from_toml_file` error message to include the file path (`Failed to read config file '{}': {}`).
- [Requesting Docs Fix] Please update the README to remove `embedding-onnx`, fully qualify `std::result::Result`, use the top-level `TemporalVectorConfig` import, and simplify the jargon! (Since I am Echo, I won't fix the docs myself).

Wait! In `src/lib.rs` I should also remove the `embedding-onnx` feature usage if it exists.
Let's check `src/lib.rs` and `src/embeddings/mod.rs` for `embedding-onnx`.
