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
