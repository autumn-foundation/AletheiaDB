🗣️ Echo: Getting Started examples are broken

## 🔎 EXPERIENCE
I am a new user who just found this database, and I am going through the "README Run". I am copy-pasting the exact examples provided in the `README.md` file into a fresh `main.rs` file to see if they compile and run correctly.

## 🚧 STUMBLE

1. **"Nova" Experimental Example:**
   "Tried to run the `story_demo` example code for Narrative Generation. The compiler said `unresolved import aletheiadb::experimental::temporal_narrative`. Why didn't the copy-pasted code include the required feature flags?"

2. **Index Persistence / Tiered Storage Examples:**
   "I ran an example that requires persistence, then tried to run another example. It crashed with `Cannot persist NodeVersion 1: VectorDelta::Sparse found for property key InternedString("embedding")`. Why do I have to manually delete `aletheiadb/` or `data/` directories to prevent state bleeding between runs?"

3. **Query Language (AQL) Example:**
   "I tried to run the Bi-temporal query (point-in-time) example, and it crashed with `InvalidParameter { parameter: "timestamp", reason: "Invalid timestamp '2024-01-15T10:00:00Z'. Expected microseconds since epoch." }`. The example uses an ISO string, but the AQL executor expects an integer."

4. **Sharding Example:**
   "I copy-pasted the Graph Sharding example, and it failed to compile because I didn't have the `sharding-rpc` feature enabled. Same issue as the Nova example."

## 📢 REPORT

* 🤦 **The Confusion:** The `README.md` code snippets often don't run cleanly when copy-pasted due to missing feature flags in the code blocks themselves, state bleeding from previous runs, or outdated syntax (like AQL timestamps).
* 🕵️ **The Reality:**
  - To use experimental or sharding modules, users must explicitly configure `Cargo.toml`.
  - State persists locally by default, causing weird crashes if you're just testing different examples in the same directory.
  - The AQL parser expects timestamps as microseconds since epoch, not ISO strings.
* 💡 **The Fix:**
  - Update the README examples to include comments within the code block that explicitly specify the required `Cargo.toml` dependencies (e.g., `// ⚠️ REQUIRES FEATURE: nova`).
  - Add a visible warning about state persistence and recommend using temporary directories or clearing state when testing.
  - Fix the AQL example to pass a valid timestamp format (microseconds).

## 🧪 VERIFY
Ensure that fixing the AQL query format and adding explicit feature/state warnings does not cause the examples to become overly verbose or lose their "Simple is better than Powerful" appeal.