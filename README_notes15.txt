Ah, `pub use crate::core::error::{Error, Result};` is exported in prelude.
So the "generic argument mismatch" error happens if they use `Result<(), Box<dyn Error>>` while `Result` takes only one argument `T`. But since I can't change the docs, I can change the signature of `aletheiadb::core::error::Result`? No, the memory explicitly states:
"In AletheiaDB, the `core::error` module defines a custom `Result<T>` alias (`pub type Result<T> = std::result::Result<T, Error>`). When defining functions that return a different error type (like `String`), use the fully qualified `std::result::Result<T, E>` to prevent `E0107` generic argument mismatch errors."
So changing `Result` alias is against the memory.

What about Vector search imports?
"Complain if I have to import 12 traits to use one struct."
Vector search uses:
```rust
use aletheiadb::{HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::TemporalVectorConfig;
```
If we re-export `TemporalVectorConfig` in `aletheiadb::prelude::*`, it makes it cleaner. Even better, re-export `HnswConfig` and `DistanceMetric` in `prelude` too.
Let's look at `src/lib.rs` to see what it exports.
