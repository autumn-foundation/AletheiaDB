# 🗣️ Echo: Getting Started examples in README are broken

## 🔎 EXPERIENCE

I am a new user trying to run the examples from the `README.md` file to learn how to use AletheiaDB. I copy-pasted the code blocks directly into a fresh `src/main.rs` file.

## 🚧 STUMBLE

1. The **"Basic Graph Operations"** example doesn't compile out of the box.
   - `AletheiaDB` and the `properties!` macro are not exported by the prelude. The compiler complains they are not found.
2. The **"Time-Travel Queries"** example also relies on `AletheiaDB` and `properties!` and fails to compile.
3. The **"Transactions"** example is missing the `PropertyMap` import.
4. The **"Vector Search with HNSW"** example returns a `similar` variable that is unused, triggering a warning.
5. The **"Query Language (AQL)"** example uses a string literal `'2024-01-15T10:00:00Z'` for the time argument in the `AS OF` clause. When I ran the exact query string, the database returned: `Error: Query(InvalidParameter { parameter: "timestamp", reason: "Invalid timestamp '2024-01-15T10:00:00Z'. Expected microseconds since epoch." })`
6. The **"Observability"** example requires `aletheiadb::observability` to be imported, but `AletheiaDB` type itself is not correctly fully qualified if it assumes it was imported via prelude (the code tries `let db = aletheiadb::AletheiaDB::new().unwrap();` without importing `AletheiaDB`).
7. The **"Embedding Generation"** example requires `tokio` to be added as a dependency, and lacks an explicit mention of the required `tokio` features (e.g., `features = ["full"]`).
8. The **"Index Persistence"** example creates a state dir (`data/my-database`) which causes subsequent examples to fail with `InvalidTimeRange` errors.

## 📣 REPORT

* 🤦 **The Confusion:** Tried to run the Getting Started code blocks. Compiler said `AletheiaDB`, `properties!`, and `PropertyMap` were not found in scope. Also, the AQL query failed at runtime with an invalid timestamp format.
* 🕵️ **The Reality:** Turns out I needed to explicitly import `AletheiaDB`, `properties`, and `PropertyMap`. `prelude::*` is basically empty! Furthermore, the AQL engine does not accept ISO strings for time, only integer strings representing microseconds since the epoch!
* 💡 **The Fix:** Add the missing imports to the code examples. Also, fix the AQL query example to use an epoch microsecond string instead of an ISO date string (or implement ISO parsing in AQL!). Finally, add the missing `tokio` config and suppress unused variables with `_`.

## 🧪 VERIFY

To verify this, I created a fresh `cargo` project and pasted the examples line-by-line. They reliably fail compilation or runtime execution. Fix the examples to eliminate friction.