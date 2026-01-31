import sys

def main():
    with open('src/query/executor/iterators.rs', 'r') as f:
        content = f.read()

    # The block to replace
    start_marker = """    fn get_neighbors(&self, node_id: NodeId) -> Vec<(NodeId, crate::core::EdgeId)> {
        // Acquire historical lock ONCE for all edge checks in this call.
        // This avoids the performance regression of acquiring per-edge locks.
        let historical_guard = self.temporal_context.map(|_| self.historical.read());

        match self.direction {"""

    end_marker = """                neighbors
            }
        }
    }
"""

    new_code = """    /// Collect and filter edges in a specific direction into a buffer.
    ///
    /// This helper reduces code duplication for Outgoing/Incoming/Both traversals
    /// while avoiding intermediate allocations by pushing to a provided buffer.
    fn collect_neighbors_in_direction(
        &self,
        node_id: NodeId,
        direction: Direction,
        historical_guard: &Option<parking_lot::RwLockReadGuard<'_, HistoricalStorage>>,
        neighbors: &mut Vec<(NodeId, crate::core::EdgeId)>,
    ) {
        match direction {
            Direction::Outgoing => {
                if let Some(ref label) = self.label {
                    neighbors.extend(
                        self.current
                            .get_outgoing_edges_with_label_iter(node_id, label)
                            .filter_map(|edge_id| {
                                if !self.edge_visible_at_time(edge_id, historical_guard) {
                                    return None;
                                }
                                self.current
                                    .get_edge_target(edge_id)
                                    .ok()
                                    .map(|target| (target, edge_id))
                            }),
                    );
                } else {
                    neighbors.extend(
                        self.current
                            .get_outgoing_edges_iter(node_id)
                            .filter_map(|edge_id| {
                                if !self.edge_visible_at_time(edge_id, historical_guard) {
                                    return None;
                                }
                                self.current
                                    .get_edge_target(edge_id)
                                    .ok()
                                    .map(|target| (target, edge_id))
                            }),
                    );
                }
            }
            Direction::Incoming => {
                if let Some(ref label) = self.label {
                    neighbors.extend(
                        self.current
                            .get_incoming_edges_with_label_iter(node_id, label)
                            .filter_map(|edge_id| {
                                if !self.edge_visible_at_time(edge_id, historical_guard) {
                                    return None;
                                }
                                self.current
                                    .get_edge_source(edge_id)
                                    .ok()
                                    .map(|source| (source, edge_id))
                            }),
                    );
                } else {
                    neighbors.extend(
                        self.current
                            .get_incoming_edges_iter(node_id)
                            .filter_map(|edge_id| {
                                if !self.edge_visible_at_time(edge_id, historical_guard) {
                                    return None;
                                }
                                self.current
                                    .get_edge_source(edge_id)
                                    .ok()
                                    .map(|source| (source, edge_id))
                            }),
                    );
                }
            }
            Direction::Both => {
                self.collect_neighbors_in_direction(
                    node_id,
                    Direction::Outgoing,
                    historical_guard,
                    neighbors,
                );
                self.collect_neighbors_in_direction(
                    node_id,
                    Direction::Incoming,
                    historical_guard,
                    neighbors,
                );
            }
        }
    }

    fn get_neighbors(&self, node_id: NodeId) -> Vec<(NodeId, crate::core::EdgeId)> {
        // Acquire historical lock ONCE for all edge checks in this call.
        // This avoids the performance regression of acquiring per-edge locks.
        let historical_guard = self.temporal_context.map(|_| self.historical.read());
        let mut neighbors = Vec::new();
        self.collect_neighbors_in_direction(
            node_id,
            self.direction,
            &historical_guard,
            &mut neighbors,
        );
        neighbors
    }
"""

    start_idx = content.find(start_marker)
    if start_idx == -1:
        print("Start marker not found")
        sys.exit(1)

    end_idx = content.find(end_marker, start_idx)
    if end_idx == -1:
        print("End marker not found")
        sys.exit(1)

    final_content = content[:start_idx] + new_code + content[end_idx + len(end_marker):]

    with open('src/query/executor/iterators.rs', 'w') as f:
        f.write(final_content)

    print("Successfully replaced content")

if __name__ == "__main__":
    main()
