# 🗣️ Echo: Getting Started example is broken

🤦 **The Confusion:**
I tried to copy-paste the "Embedding Generation (Optional)" code block from `README.md` into a fresh `main.rs` and run it.
It completely failed to compile with `error[E0433]: failed to resolve: use of unresolved module or unlinked crate tokio`.
So I added `tokio` to my `Cargo.toml`.
Then I ran it again and got `error[E0282]: type annotations needed` on `let embeddings = service.embed_batch(&documents).await?;`.
Then I fixed that, and it crashed at runtime with `Error: ConfigError("OPENAI_API_KEY environment variable not set")`.

🕵️ **The Reality:**
The example in the README leaves out critical pieces of information for someone who just wants to copy, paste, and run.
1. It doesn't explicitly tell me I need the `tokio` features like `macros` or `rt-multi-thread` in my `Cargo.toml`.
2. The `service.embed_batch` method cannot infer its return type correctly in this context, leaving the compiler to complain.
3. The code expects `OPENAI_API_KEY` to be set in the environment, but doesn't mention it in the README snippet, so it just crashes on startup.

💡 **The Fix:**
1. Update the `README.md`'s Embedding Generation example to show the exact `Cargo.toml` dependencies needed (e.g. `tokio = { version = "1.0", features = ["macros", "rt-multi-thread"] }`).
2. Add a type annotation in the code block: `let embeddings: Vec<Vec<f32>> = service.embed_batch(&documents).await?;`.
3. Add a comment above the `OpenAIConfig::from_env` line that says `// Requires OPENAI_API_KEY environment variable`.
