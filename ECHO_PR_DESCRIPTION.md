🤦‍♂️ **The Confusion:** Tried to run the `story_demo` example by copy-pasting the README command `cargo run --example story_demo --features nova`. Cargo complained: `error: target 'story_demo' in package 'aletheiadb' requires the features: 'semantic-temporal'`.

🕵️‍♂️ **The Reality:** The `Cargo.toml` explicitly requires the `semantic-temporal` feature for this target, not `nova`. While `nova` might be an umbrella flag, Cargo's `--example` runner requires the exact feature listed in `required-features` to be passed.

💡 **The Fix:** I updated `README.md` and `examples/story_demo.rs` to use the correct command (`--features semantic-temporal`) and updated the `Cargo.toml` example snippet to use the specific feature `semantic-temporal` to avoid confusion.
