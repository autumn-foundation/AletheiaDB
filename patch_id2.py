with open("src/core/id.rs", "r") as f:
    content = f.read()

target = """#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, bytemuck::Pod, bytemuck::Zeroable,
)]
#[repr(transparent)]
pub struct NodeId(u64);"""

replacement = """/// Unique identifier for a node in the graph.
///
/// # The Context
/// In AletheiaDB, every node must have a unique identifier. This `NodeId` struct is a
/// strongly-typed wrapper around a raw `u64`. This prevents accidentally passing a
/// raw integer or an `EdgeId` to an API expecting a node ID.
///
/// # Examples
/// ```rust
/// use aletheiadb::core::id::NodeId;
///
/// // Create a valid node ID
/// let id = NodeId::new(42).unwrap();
/// assert_eq!(id.as_u64(), 42);
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, bytemuck::Pod, bytemuck::Zeroable,
)]
#[repr(transparent)]
pub struct NodeId(u64);"""

content = content.replace(target, replacement)

with open("src/core/id.rs", "w") as f:
    f.write(content)
