# Echo's DX Audit Report: `story_demo` example is broken without `nova` feature

## The Walkthrough
**Scenario:** I am a new user trying to add the `Nova` story feature.
**Action:** I copied the `story_demo` example code directly into a new Rust project `main.rs` file and ran `cargo run` using the basic `AletheiaDB` dependency without any feature flags.

## The Friction Points
1. `story_demo` example compilation fails if the `nova` or `semantic-temporal` features are not enabled.
2. The example code imports `aletheiadb::experimental::temporal_narrative::NarrativeGenerator`, which is conditionally compiled when `semantic-temporal` is enabled. Without the feature flag, `temporal_narrative` does not exist in `experimental`.
3. Even if someone found the internal `NarrativeGenerator` without the feature flag, creating it with `.new()` would panic at runtime with an error: "NarrativeGenerator requires the 'nova' feature. Add 'features = ["nova"]' to your Cargo.toml."

## The Complaint

**Title:** 🗣️ Echo: Getting Started `story_demo` example is broken without the `nova` feature

- 🤦 **The Confusion:** Tried to run the `story_demo` from the examples, but the compiler said `temporal_narrative` was not found in `experimental`. I copied it exactly as it appeared but it wouldn't compile.
- 🕵️ **The Reality:** Turns out I needed to enable feature `nova` or `semantic-temporal` to get `temporal_narrative` to show up in the `experimental` module. The README did not say this prominently in the Quick Start or example code itself.
- 💡 **The Fix:** Add a note to the README Quick Start, and ensure the `temporal_narrative` module in `experimental/mod.rs` is exported *even when the feature is disabled*, so the helpful panic message in the stub implementation can actually be seen instead of a compilation error.

## The "idiot proofing" Verification
If we expose `temporal_narrative` without the feature flag (perhaps via a stub module), users will hit the deliberate runtime panic message guiding them to add the `nova` feature, instead of a confusing `unresolved import` compiler error. This improves the DX by holding the user's hand toward the right feature flag.
