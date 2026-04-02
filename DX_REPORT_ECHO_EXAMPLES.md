# 🗣️ Echo: Getting Started examples in README are broken

## 🤦 The Confusion
I copied the code blocks from `README.md` into my own project to see how to use AletheiaDB, and multiple examples wouldn't even compile!
- The "Transactions" example threw errors about missing variables (`alice_id` wasn't defined) and `Result` type alias conflicts.
- The "Graph Sharding" example threw errors because it didn't have a `main` function to wrap the statements, so `let` wasn't allowed for global scope.
- The "Tiered Storage" example threw errors because it returned `Result` from the global scope without being in a function.
- The "Embedding Generation (Optional)" example threw errors because `service.embed_batch(&documents).await?` needed type annotations for `embeddings` because it was being zipped with `documents` which has an unknown type at that point.
- The "Production Observability (Optional)" example threw a warning about an unused variable `db`.

## 🕵️ The Reality
The code blocks were written as snippets instead of compilable, standalone examples. Rust requires a `main` function for executable programs, and `Result` type aliases can conflict if not fully qualified or correctly used.

## 💡 The Fix
I directly modified the `README.md` to ensure the examples compile successfully. Specifically:
- Added a `main` function wrapper and `alice_id` to the "Transactions" example, and prefixed the result with `_` to avoid unused variable warnings.
- Added a `main` function wrapper to the "Graph Sharding" example.
- Added a `main` function wrapper to the "Tiered Storage" example.
- Added explicit type annotations `Vec<Vec<f32>>` to `embeddings` in the "Embedding Generation" example.
- Prefixed `db` with an underscore (`_db`) in the "Production Observability" example to prevent the warning.
