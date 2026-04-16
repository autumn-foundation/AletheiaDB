with open("src/experimental/chronos.rs", "r") as f:
    content = f.read()

target = """#[cfg(feature = "nova")]
/// The Time Lord of the Graph.
pub struct Chronos<'a> {"""
replacement = """#[cfg(feature = "nova")]
/// The Time Lord of the Graph.
///
/// This struct provides advanced temporal pathfinding and volatility analysis.
/// It acts as a wrapper over the core `AletheiaDB` instance.
pub struct Chronos<'a> {"""

content = content.replace(target, replacement)

with open("src/experimental/chronos.rs", "w") as f:
    f.write(content)
