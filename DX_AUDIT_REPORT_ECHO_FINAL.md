# DX Audit Report - Getting Started Guide (Echo 🗣️)

## 🔎 EXPERIENCE

I am a new user trying to follow the AletheiaDB `README.md` to get started with the database and test out the examples.

I am walking through the copy-pasted `README.md` examples block by block.

## 🚧 STUMBLE

### 1. The "Basic Graph Operations" example doesn't compile out of the box.

The README copy-paste block starts with:
```rust
use aletheiadb::prelude::*;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let db = AletheiaDB::new().unwrap();
```
But `AletheiaDB` and the `properties!` macro are NOT in the prelude.
I got:
```
error: cannot find macro `properties` in this scope
error[E0433]: failed to resolve: use of undeclared type `AletheiaDB`
```

### 2. Time-Travel Queries Example requires importing `time` module which isn't specified in the code block.

The example has:
```rust
use aletheiadb::prelude::*;
use aletheiadb::time;
```
It still doesn't have `AletheiaDB` and `properties!`.

### 3. Vector Search with HNSW Example throws unused variable warnings.

```rust
let similar = db.find_similar(doc_id, 10)?;
```
Throws `warning: unused variable: similar`.

### 4. Semantic Drift Tracking Example has a minor bug in imports and logic?

The example specifies:
```rust
use aletheiadb::index::vector::temporal::{DriftMetric, TemporalVectorConfig, SnapshotStrategy};
```
But it also misses `AletheiaDB` and `properties!`.

### 5. Transactions Example lacks `PropertyMap` import

```rust
use aletheiadb::prelude::*;

let alice_id = db.create_node("Event", PropertyMap::new())?;
```
`PropertyMap` is not in prelude, neither is `AletheiaDB`.

### 6. AQL Time formatting is VERY confusing.

The timestamp string in the AQL query:
```rust
"AS OF '2024-01-15T10:00:00Z' MATCH (n:Person {name: 'Alice'}) RETURN n"
```
gives:
```
Error: Query(InvalidParameter { parameter: "timestamp", reason: "Invalid timestamp '2024-01-15T10:00:00Z'. Expected microseconds since epoch." })
```
The syntax says `'2024-01-15T10:00:00Z'` but the parser fails saying "Expected microseconds since epoch". The query engine DOES NOT parse ISO strings out of the box, it wants a plain integer string of microseconds? The example is broken.

## 📣 REPORT

Create an Issue (or PR with a 'Docs Fix' request):

Title: "🗣️ Echo: Getting Started examples in README are broken"
Description:
* 🤦 **The Confusion:** Tried to run the examples from README. Compiler said `AletheiaDB`, `properties!`, and `PropertyMap` not found. `AS OF` AQL query crashed parsing the ISO string.
* 🕵️ **The Reality:** The `prelude::*` does not contain the core items like `AletheiaDB` and `properties!`. The AQL engine does not accept ISO strings for time, only integer strings of microseconds!
* 💡 **The Fix:** Fix the examples in `README.md` to properly import `AletheiaDB`, `properties`, and `PropertyMap`. Change the AQL query to use microsecond epoch strings or fix the underlying parser.

## 🧪 VERIFY

I will verify the underlying issue. Wait, I should not fix the code/docs myself, my directive states: "Never assume users will "figure it out", read the source code, or attempt to fix the docs/code yourself; strictly report findings."

Wait, my prompt actually says: "Your mission is to audit the "Developer Experience" (DX)... Create an Issue (or PR with a 'Docs Fix' request)... 🚫 **Never do:**... Fix the docs yourself"

Actually, let's create a PR titled '🗣️ Echo: Getting Started examples in README are broken' as a markdown report file `DX_AUDIT_REPORT.md` and NOT modify the codebase. I will just submit the report file.