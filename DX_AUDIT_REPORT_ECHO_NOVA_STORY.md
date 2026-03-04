# 🗣️ Echo: Getting Started example is broken

## 🤦 The Confusion

I am a new user trying to add `Nova`'s story feature. I copied the code snippet from the "Narrative Generation (Experimental)" section in `README.md` to a new `main.rs` file.

```rust
use aletheiadb::prelude::*;
use aletheiadb::experimental::temporal_narrative::NarrativeGenerator;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // 1. Setup database and node (for self-contained example)
    let db = AletheiaDB::new().unwrap();
    let node_id = db.write(|tx| {
        tx.create_node("Person", properties! {
            "name" => "Alice"
        })
    })?;

    // 2. Generate natural language history of a node
    let generator = NarrativeGenerator::new(&db);
    let narrative = generator.generate_node_narrative(node_id)?;

    for event in narrative {
        println!("Version {}: {}", event.version_number, event.description);
        // Output: "Version 1: Node created with label 'Person'."

        for change in event.changes {
            println!("  - {}", change);
            // Output: "  - Initial property 'name': '"Alice"'"
        }
    }

    Ok(())
}
```

When I try to run it using `cargo run`, the compiler yells at me with deprecated warnings:

```
warning: use of deprecated struct `aletheiadb::experimental::temporal_narrative::NarrativeGenerator`: NarrativeGenerator requires the 'nova' feature. Add 'features = ["nova"]' to your Cargo.toml.
 --> src/main.rs:2:51
  |
2 | use aletheiadb::experimental::temporal_narrative::NarrativeGenerator;
  |                                                   ^^^^^^^^^^^^^^^^^^
  |
  = note: `#[warn(deprecated)]` on by default

warning: use of deprecated struct `aletheiadb::experimental::temporal_narrative::NarrativeGenerator`: NarrativeGenerator requires the 'nova' feature. Add 'features = ["nova"]' to your Cargo.toml.
  --> src/main.rs:14:21
   |
14 |     let generator = NarrativeGenerator::new(&db);
   |                     ^^^^^^^^^^^^^^^^^^
```

And then when I run the built executable, it panics at runtime with:

```
thread 'main' panicked at src/experimental/temporal_narrative.rs:135:9:
NarrativeGenerator requires the 'nova' feature. Add 'features = ["nova"]' to your Cargo.toml.
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

The example uses code that compiles and runs correctly when the feature is enabled but fails with cryptic warnings and panics otherwise. The `nova` requirement is written as a Rust comment in the README, which people just skip over!

## 🕵️ The Reality

Turns out I needed to enable the `nova` feature in `Cargo.toml`.

But wait, if the feature is experimental, why are its modules (`temporal_narrative`) exposed by default but completely useless and prone to panic when called without the feature? The "deprecated struct" message is extremely confusing for a feature I just found. The comment block at the start of the `README.md` block `// ⚠️ REQUIRES FEATURE: nova` doesn't stop people from just copying the imports and the logic, because no one reads comments.

Also, when I try to copy the *Basic Graph Operations* snippet in `README.md`, if I don't use `cargo`, but just `rustc`, the `properties!` macro doesn't work out of the box unless I use `--extern` and link it, but I digress... I am using cargo.

The `NarrativeGenerator` isn't actually "deprecated", it's a stub that panics when the `nova` feature isn't active.

## 💡 The Fix

1. Add a huge banner in `README.md` right before the "Narrative Generation (Experimental)" section saying: `> **REQUIRES FEATURE NOVA**`. And please make it clear that the code *will* compile but *will* panic if the feature is not in `Cargo.toml`.
2. Reconsider exporting `aletheiadb::experimental::temporal_narrative::NarrativeGenerator` as a panic-stub when `nova` is not enabled. It shouldn't exist in the public API unless `nova` is enabled, or at least the compiler should throw a hard error (`#[cfg(feature = "nova")]` on the module itself) rather than just a deprecation warning that leads to runtime panics. Failing to compile with "module not found" is a much clearer sign than compiling with a "deprecated" warning and panicking!