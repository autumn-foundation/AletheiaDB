import sys

with open("src/experimental/sybil.rs", "r") as f:
    content = f.read()

to_remove = """/// Trait for defining how a node's state updates based on its neighbors.
pub trait PropagationModel {
    /// Calculate the next state for a node.
    ///
    /// # Arguments
    /// * `node_id` - The node being updated.
    /// * `current_self` - The node's current vector (if any).
    /// * `neighbors` - List of (NodeId, Neighbor Vector) tuples.
    ///
    /// # Returns
    /// The new vector state for the node.
    fn next_state(
        &self,
        node_id: NodeId,
        current_self: Option<&[f32]>,
        neighbors: &[(NodeId, &[f32])],
    ) -> Option<Vec<f32>>;
}

"""
content = content.replace(to_remove, "")
content = content.replace("impl PropagationModel for LinearPropagation {", "impl LinearPropagation {")
content = content.replace("    fn next_state(", "    pub fn next_state(")
content = content.replace("pub fn simulate<M: PropagationModel>(", "pub fn simulate(")
content = content.replace("model: &M,", "model: &LinearPropagation,")

with open("src/experimental/sybil.rs", "w") as f:
    f.write(content)
