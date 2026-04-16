with open("src/experimental/chronos.rs", "r") as f:
    content = f.read()

target = """#[cfg(not(feature = "nova"))]
#[deprecated(
    note = "Chronos requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
)]
pub struct Chronos<'a> {"""

replacement = """#[cfg(not(feature = "nova"))]
/// The Time Lord of the Graph (Stub).
///
/// This is a stub implementation provided when the `nova` feature is disabled.
/// Chronos requires the `nova` experimental feature flag to compile its true
/// implementation due to its heavy reliance on bleeding-edge temporal graph APIs.
///
/// # Note
/// Attempting to use this without the `nova` feature will trigger deprecation warnings
/// and will panic if you try to instantiate it.
#[deprecated(
    note = "Chronos requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
)]
pub struct Chronos<'a> {"""

content = content.replace(target, replacement)

with open("src/experimental/chronos.rs", "w") as f:
    f.write(content)
