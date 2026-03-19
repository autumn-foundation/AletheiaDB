# 🗣️ Echo: Getting Started example is broken

## Description

🤦 **The Confusion:**
I was following the "Narrative Generation (Experimental)" example in the `README.md`. I copy-pasted the exact code block into my fresh `main.rs` and ran it. My compiler yelled at me:
```
warning: use of deprecated struct `aletheiadb::experimental::temporal_narrative::NarrativeGenerator`: NarrativeGenerator requires the 'nova' feature. Add 'features = ["nova"]' to your Cargo.toml.
```
Then, when I tried to run it anyway, it just crashed right in my face:
```
thread 'main' panicked at src/bin/test_story_demo.rs:14:21:
NarrativeGenerator requires the 'nova' feature. Add 'features = ["nova"]' to your Cargo.toml.
```
If I have to read the source code or compiler warnings to figure out why the "Getting Started" example doesn't work out of the box, the documentation failed. If I copy-paste the example and it doesn't compile or run, I am leaving.

🕵️ **The Reality:**
Turns out, `NarrativeGenerator` is hidden behind an experimental `nova` feature flag. The README has comments saying `// ⚠️ REQUIRES FEATURE: nova` and `// aletheiadb = { version = "0.1", features = ["nova"] }`, but comments aren't code, and when I blindly copy-paste the Rust code and `cargo run`, the Rust code is actually compiled with a stub that panics at runtime if the feature isn't enabled in my `Cargo.toml`.

💡 **The Fix:**
Add a huge, unmistakable banner or note in the README section *before* the code block that explicitly warns users to update their `Cargo.toml` with `features = ["nova"]` before they even try to copy the code. The warning needs to be in the Markdown text, not just hidden in the code comments. Also, maybe don't make the stub compile just to panic at runtime—if it's missing the feature, make it a hard compile error so I don't get false hope!

# 🗣️ Echo: Query Language (AQL) AS OF example is broken

## Description

🔎 **EXPERIENCE:**
I am a new user trying to use the bi-temporal query features. I found the `execute_aql` example under "Query Language (AQL)" in the `README.md`. It showed a cool "point-in-time" query using an ISO 8601 string:
`"AS OF '2024-01-15T10:00:00Z' MATCH (n:Person {name: 'Alice'}) RETURN n"`
I copy-pasted this code directly into my `main.rs` and ran it.

🚧 **STUMBLE:**
It compiled fine, but when I ran it, it crashed with this error:
`Error: Query(InvalidParameter { parameter: "timestamp", reason: "Invalid timestamp '2024-01-15T10:00:00Z'. Expected microseconds since epoch." })`
The documentation explicitly shows using a human-readable ISO 8601 date string, but the actual parser rejects it and demands an integer (microseconds since epoch). If I copy-paste the exact code from the README and it crashes at runtime with a parsing error, the documentation failed. I don't want to calculate microseconds since epoch in my head.

📣 **REPORT:**
The `README.md` example for `execute_aql` is broken because the AQL parser doesn't actually support the ISO 8601 string format it advertises in the docs.
- **Fix 1 (Easy):** Update the `README.md` example to use an integer timestamp so the code actually runs out of the box.
- **Fix 2 (Better):** Make the parser actually accept ISO 8601 strings in the `AS OF` clause, since that's what users will naturally want to type!

🧪 **VERIFY:**
If the documentation is updated, I will copy-paste the new example and verify it runs without panicking. If the parser is updated instead, I will verify the existing `AS OF '2024-01-15T10:00:00Z'` query works.
